use serde::{Deserialize, Serialize};
use std::{
    env,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub const PROTOCOL_VERSION: u32 = 1;
pub const PYTHON_ENV: &str = "CAR_LOGGER_AI_PYTHON";
pub const WORKER_SCRIPT_ENV: &str = "CAR_LOGGER_AI_WORKER_SCRIPT";

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WorkerDiagnostic {
    pub python_version: String,
    pub tensorflow_version: String,
    pub keras_version: String,
    pub cpu: String,
    pub memory_bytes: Option<u64>,
    pub writable: bool,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLaunch {
    pub python: PathBuf,
    pub script: PathBuf,
}

impl WorkerLaunch {
    /// Finds the isolated worker runtime. Explicit environment settings win;
    /// source-tree and application-adjacent layouts are fallback candidates.
    pub fn discover() -> Result<Self, WorkerError> {
        let roots = runtime_roots();
        let python = if let Some(value) = env::var_os(PYTHON_ENV).filter(|x| !x.is_empty()) {
            require_file(PathBuf::from(value), PYTHON_ENV)?
        } else {
            let candidates = roots.iter().flat_map(|root| {
                [
                    root.join("python/ai_worker/.venv/bin/python"),
                    root.join("python/ai_worker/.venv/Scripts/python.exe"),
                ]
            });
            first_file(candidates).ok_or_else(|| WorkerError::RuntimeNotFound {
                component: "TensorFlow Python virtual environment".into(),
                hint: format!("set {PYTHON_ENV} to python/ai_worker/.venv/bin/python"),
            })?
        };
        let script = if let Some(value) = env::var_os(WORKER_SCRIPT_ENV).filter(|x| !x.is_empty()) {
            require_file(PathBuf::from(value), WORKER_SCRIPT_ENV)?
        } else {
            first_file(
                roots
                    .iter()
                    .map(|root| root.join("python/ai_worker/run_worker.py")),
            )
            .ok_or_else(|| WorkerError::RuntimeNotFound {
                component: "AI worker script".into(),
                hint: format!("set {WORKER_SCRIPT_ENV} to run_worker.py"),
            })?
        };
        Ok(Self { python, script })
    }

    pub fn spawn_verified(
        &self,
        data_dir: &Path,
        timeout: Duration,
    ) -> Result<(Worker, WorkerDiagnostic), WorkerError> {
        let mut worker = Worker::spawn(&self.python, &self.script, data_dir)?;
        let response = worker.request("health_check", serde_json::json!({}), timeout)?;
        if !response.ok {
            return Err(WorkerError::Unhealthy(
                response
                    .error
                    .unwrap_or_else(|| "unknown worker error".into()),
            ));
        }
        let diagnostic: WorkerDiagnostic = serde_json::from_value(response.payload)
            .map_err(|error| WorkerError::Protocol(error.to_string()))?;
        if diagnostic.protocol_version != PROTOCOL_VERSION || !diagnostic.writable {
            return Err(WorkerError::Unhealthy(
                "worker diagnostic is incompatible or data directory is not writable".into(),
            ));
        }
        Ok((worker, diagnostic))
    }
}

fn first_file(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|path| path.is_file())
}

fn require_file(path: PathBuf, setting: &str) -> Result<PathBuf, WorkerError> {
    if path.is_file() {
        Ok(path)
    } else {
        Err(WorkerError::RuntimeNotFound {
            component: setting.into(),
            hint: format!("{} does not exist", path.display()),
        })
    }
}

fn runtime_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
    ];
    if let Ok(executable) = env::current_exe() {
        roots.extend(executable.ancestors().map(Path::to_path_buf));
    }
    roots.dedup();
    roots
}
#[derive(Debug, Serialize)]
struct Request<'a> {
    request_id: &'a str,
    protocol_version: u32,
    kind: &'a str,
    payload: serde_json::Value,
}
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Response {
    pub request_id: String,
    pub protocol_version: u32,
    pub kind: String,
    pub ok: bool,
    pub payload: serde_json::Value,
    pub error: Option<String>,
}
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("worker protocol: {0}")]
    Protocol(String),
    #[error("worker timed out")]
    Timeout,
    #[error("worker exited")]
    Exited,
    #[error("AI runtime not found: {component}; {hint}")]
    RuntimeNotFound { component: String, hint: String },
    #[error("AI worker health check failed: {0}")]
    Unhealthy(String),
}

pub struct Worker {
    child: Child,
    stdin: ChildStdin,
    responses: mpsc::Receiver<Result<Response, WorkerError>>,
}

/// Prevents a crash loop from competing with logging for CPU and memory.
/// Three failures inside the configured interval disable AI until a new session.
#[derive(Debug)]
pub struct CrashGuard {
    failures: std::collections::VecDeque<std::time::Instant>,
    interval: Duration,
    disabled: bool,
}
impl CrashGuard {
    pub fn new(interval: Duration) -> Self {
        Self {
            failures: Default::default(),
            interval,
            disabled: false,
        }
    }
    pub fn record_failure(&mut self, now: std::time::Instant) -> bool {
        while self
            .failures
            .front()
            .is_some_and(|at| now.duration_since(*at) > self.interval)
        {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        if self.failures.len() >= 3 {
            self.disabled = true;
        }
        self.disabled
    }
    pub fn disabled(&self) -> bool {
        self.disabled
    }
}

#[derive(Debug)]
pub struct InferenceRequest {
    pub payload: serde_json::Value,
    pub timeout: Duration,
    pub reply: mpsc::Sender<Result<Response, WorkerError>>,
}

/// Owns the long-lived worker on a dedicated thread. A bounded queue provides
/// backpressure, so CAN/OBD polling and the GUI never wait for TensorFlow.
pub struct AsyncInferenceWorker {
    sender: mpsc::SyncSender<InferenceRequest>,
    handle: Option<thread::JoinHandle<()>>,
}
impl AsyncInferenceWorker {
    pub fn spawn(mut worker: Worker, queue_capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<InferenceRequest>(queue_capacity);
        let handle = thread::spawn(move || {
            for request in receiver {
                let result = worker.request("infer", request.payload, request.timeout);
                let _ = request.reply.send(result);
            }
        });
        Self {
            sender,
            handle: Some(handle),
        }
    }
    pub fn try_infer(
        &self,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<mpsc::Receiver<Result<Response, WorkerError>>, WorkerError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.sender
            .try_send(InferenceRequest {
                payload,
                timeout,
                reply: reply_tx,
            })
            .map_err(|e| WorkerError::Protocol(format!("inference queue unavailable: {e}")))?;
        Ok(reply_rx)
    }
}
impl Drop for AsyncInferenceWorker {
    fn drop(&mut self) {
        let (replacement, _) = mpsc::sync_channel(0);
        let old = std::mem::replace(&mut self.sender, replacement);
        drop(old);
        let _ = self.handle.take();
    }
}
impl Worker {
    pub fn spawn_discovered(
        data_dir: &Path,
        timeout: Duration,
    ) -> Result<(Self, WorkerDiagnostic), WorkerError> {
        WorkerLaunch::discover()?.spawn_verified(data_dir, timeout)
    }

    pub fn spawn(python: &Path, script: &Path, data_dir: &Path) -> Result<Self, WorkerError> {
        std::fs::create_dir_all(data_dir)?;
        set_private(data_dir)?;
        let mut child = Command::new(python)
            .arg(script)
            .arg("--data-dir")
            .arg(data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| WorkerError::Protocol("stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WorkerError::Protocol("stdout unavailable".into()))?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let result = line.map_err(WorkerError::Io).and_then(|s| {
                    serde_json::from_str(&s).map_err(|e| WorkerError::Protocol(e.to_string()))
                });
                if tx.send(result).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            responses: rx,
        })
    }
    pub fn request(
        &mut self,
        kind: &str,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<Response, WorkerError> {
        if self.child.try_wait()?.is_some() {
            return Err(WorkerError::Exited);
        }
        let id = uuid::Uuid::new_v4().to_string();
        serde_json::to_writer(
            &mut self.stdin,
            &Request {
                request_id: &id,
                protocol_version: PROTOCOL_VERSION,
                kind,
                payload,
            },
        )
        .map_err(|e| WorkerError::Protocol(e.to_string()))?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        let response = self.responses.recv_timeout(timeout).map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => WorkerError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => WorkerError::Exited,
        })??;
        if response.request_id != id {
            return Err(WorkerError::Protocol("request_id mismatch".into()));
        }
        if response.protocol_version != PROTOCOL_VERSION {
            return Err(WorkerError::Protocol("protocol version mismatch".into()));
        }
        Ok(response)
    }
    pub fn shutdown(mut self, timeout: Duration) -> Result<(), WorkerError> {
        let _ = self.request("shutdown", serde_json::json!({}), timeout)?;
        let _ = self.child.wait()?;
        Ok(())
    }
    pub fn cancel(&mut self, request_id: &str, timeout: Duration) -> Result<Response, WorkerError> {
        self.request(
            "cancel",
            serde_json::json!({"target_request_id":request_id}),
            timeout,
        )
    }

    /// Immediately stops an in-flight training process. Candidate directories
    /// are never current-model pointers, so forced interruption is adoption-safe.
    pub fn terminate(&mut self) -> Result<(), WorkerError> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
            let _ = self.child.wait()?;
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
    }
}
#[cfg(unix)]
fn set_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}
#[cfg(not(unix))]
fn set_private(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn fake(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("w.py");
        fs::write(&p, body).unwrap();
        (d, p)
    }
    #[test]
    fn request_response_and_shutdown() {
        let (d, p) = fake(
            "import sys,json\nfor l in sys.stdin:\n r=json.loads(l); print(json.dumps({'request_id':r['request_id'],'protocol_version':1,'kind':r['kind'],'ok':True,'payload':{},'error':None}),flush=True)\n if r['kind']=='shutdown': break\n",
        );
        let mut w = Worker::spawn(Path::new("python3"), &p, d.path()).unwrap();
        assert!(
            w.request("health", serde_json::json!({}), Duration::from_secs(2))
                .unwrap()
                .ok
        );
        w.shutdown(Duration::from_secs(2)).unwrap();
    }
    #[test]
    fn timeout_and_crash() {
        let (d, p) = fake("import time;time.sleep(5)");
        let mut w = Worker::spawn(Path::new("python3"), &p, d.path()).unwrap();
        assert!(matches!(
            w.request("x", serde_json::json!({}), Duration::from_millis(20)),
            Err(WorkerError::Timeout)
        ));
        drop(w);
    }
    #[test]
    fn protocol_mismatch() {
        let (d, p) = fake(
            "import sys,json\nr=json.loads(input());print(json.dumps({'request_id':r['request_id'],'protocol_version':2,'kind':'x','ok':True,'payload':{},'error':None}),flush=True)",
        );
        let mut w = Worker::spawn(Path::new("python3"), &p, d.path()).unwrap();
        assert!(matches!(
            w.request("x", serde_json::json!({}), Duration::from_secs(2)),
            Err(WorkerError::Protocol(_))
        ));
    }

    #[test]
    fn forced_termination_is_safe_and_idempotent() {
        let (d, p) = fake("import time;time.sleep(5)");
        let mut w = Worker::spawn(Path::new("python3"), &p, d.path()).unwrap();
        w.terminate().unwrap();
        w.terminate().unwrap();
        assert!(matches!(
            w.request("x", serde_json::json!({}), Duration::from_millis(20)),
            Err(WorkerError::Exited)
        ));
    }

    #[test]
    fn verified_launch_returns_typed_diagnostic() {
        let (d, p) = fake(
            "import sys,json\nfor l in sys.stdin:\n r=json.loads(l); payload={'python_version':'3.13','tensorflow_version':'2.21.0','keras_version':'3','cpu':'test','memory_bytes':123,'writable':True,'protocol_version':1}; print(json.dumps({'request_id':r['request_id'],'protocol_version':1,'kind':r['kind'],'ok':True,'payload':payload,'error':None}),flush=True)\n if r['kind']=='shutdown': break\n",
        );
        let launch = WorkerLaunch {
            python: PathBuf::from("python3"),
            script: p,
        };
        let (worker, diagnostic) = launch
            .spawn_verified(d.path(), Duration::from_secs(2))
            .unwrap();
        assert_eq!(diagnostic.tensorflow_version, "2.21.0");
        worker.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn source_tree_discovery_prefers_project_virtual_environment() {
        match WorkerLaunch::discover() {
            Ok(launch) => {
                assert!(launch.python.ends_with("python/ai_worker/.venv/bin/python"));
                assert!(launch.script.ends_with("python/ai_worker/run_worker.py"));
            }
            Err(WorkerError::RuntimeNotFound { .. }) => {}
            Err(error) => panic!("unexpected discovery error: {error}"),
        }
    }
    #[test]
    fn three_rapid_crashes_disable_session_ai() {
        let mut guard = CrashGuard::new(Duration::from_secs(60));
        let now = std::time::Instant::now();
        assert!(!guard.record_failure(now));
        assert!(!guard.record_failure(now + Duration::from_secs(1)));
        assert!(guard.record_failure(now + Duration::from_secs(2)));
        assert!(guard.disabled());
    }
    #[test]
    fn inference_submission_does_not_wait_for_tensorflow() {
        let (d, p) = fake(
            "import sys,json,time\nfor l in sys.stdin:\n r=json.loads(l);time.sleep(.2);print(json.dumps({'request_id':r['request_id'],'protocol_version':1,'kind':'infer','ok':True,'payload':{},'error':None}),flush=True)\n",
        );
        let worker = Worker::spawn(Path::new("python3"), &p, d.path()).unwrap();
        let async_worker = AsyncInferenceWorker::spawn(worker, 1);
        let started = std::time::Instant::now();
        let response = async_worker
            .try_infer(serde_json::json!({}), Duration::from_secs(1))
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(
            response
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .ok
        );
    }
}
