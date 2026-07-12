use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub const PROTOCOL_VERSION: u32 = 1;
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
}

pub struct Worker {
    child: Child,
    stdin: ChildStdin,
    responses: mpsc::Receiver<Result<Response, WorkerError>>,
}
impl Worker {
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
}
