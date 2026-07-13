use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver as MpscReceiver, SyncSender, sync_channel};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use car_logger_ai_worker::{Response, Worker};
use car_logger_application::RealtimeState;
use car_logger_application::pid_scan::{PidScanConfig, PidScanProgress};
use car_logger_application::{
    CanFrameSource, DiagnosticRepository, HealthScoreRepository, ScoreGranularity,
};
use car_logger_domain::{CanFrame, SignalKind};
use car_logger_health::ai_condition::{
    AiAvailability, AiWindowResult, evaluate_session, overall_condition,
};
use car_logger_health::ai_features::{
    AiFeatureContract, FeatureWindow, RawSignalSample, RealtimeAiCollector,
    build_windows_with_contract, canonical_signal_key, feature_contract,
};
use car_logger_storage::ai::OverallConditionRecord;
use car_logger_storage::ai::{ModelGenerationRecord, SessionSplit, TrainingReadiness};
use car_logger_storage::{DuckdbCanFrameRepository, SqliteMasterRepository};
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::signal_decoder::{SignalDefinitionMap, decode_frame};

const FLUSH_INTERVAL: Duration = Duration::from_millis(500);
const RECEIVE_ERROR_INTERVAL: Duration = Duration::from_secs(2);
const SAVE_BATCH_SIZE: usize = 128;
const MAX_BUFFERED_FRAMES: usize = 10_000;

struct RealtimeAiRuntime {
    windows: SyncSender<FeatureWindow>,
    results: MpscReceiver<Result<AiWindowResult, String>>,
}

fn parse_inference_response(
    response: Response,
    request_id: String,
    window_start: chrono::DateTime<chrono::Utc>,
) -> Result<AiWindowResult, String> {
    if !response.ok {
        return Err(response
            .error
            .unwrap_or_else(|| "AI inference failed".into()));
    }
    let mut payload = response.payload;
    let object = payload
        .as_object_mut()
        .ok_or_else(|| "AI inference returned a non-object".to_string())?;
    object.insert("request_id".into(), serde_json::Value::String(request_id));
    object.insert(
        "window_start".into(),
        serde_json::Value::String(window_start.to_rfc3339()),
    );
    object.insert(
        "availability".into(),
        serde_json::Value::String("available".into()),
    );
    serde_json::from_value(payload).map_err(|error| error.to_string())
}

impl RealtimeAiRuntime {
    fn spawn(
        data_dir: PathBuf,
        model_id: String,
        scope: String,
        contract: AiFeatureContract,
    ) -> Self {
        let (window_tx, window_rx) = sync_channel::<FeatureWindow>(2);
        let (result_tx, result_rx) = sync_channel(8);
        thread::spawn(move || {
            let mut worker = match Worker::spawn_discovered(&data_dir, Duration::from_secs(30)) {
                Ok((worker, _)) => worker,
                Err(error) => {
                    let _ = result_tx.send(Err(error.to_string()));
                    return;
                }
            };
            for window in window_rx {
                let values = (0..60)
                    .map(|time| {
                        window
                            .values
                            .iter()
                            .map(|signal| signal[time])
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                let masks = (0..60)
                    .map(|time| {
                        window
                            .observed_mask
                            .iter()
                            .map(|signal| signal[time])
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                let request_id = format!("{}-{}", model_id, window.started_at.timestamp_millis());
                let request = serde_json::json!({
                    "model_id":&model_id, "scope": &scope, "feature_schema":&contract.schema_version,
                    "values":values, "masks":masks, "signal_keys":&contract.signal_keys,
                    "driving_state":serde_json::to_value(window.state).unwrap_or_default(),
                    "window_start":window.started_at.to_rfc3339(),
                });
                let parsed = worker
                    .request("infer", request, Duration::from_secs(5))
                    .map_err(|error| error.to_string())
                    .and_then(|response| {
                        parse_inference_response(response, request_id, window.started_at)
                    });
                let _ = result_tx.send(parsed);
            }
        });
        Self {
            windows: window_tx,
            results: result_rx,
        }
    }
}

fn spawn_automatic_training(
    database_path: PathBuf,
    data_dir: PathBuf,
    vehicle_id: i64,
    split: SessionSplit,
    schema: String,
) {
    thread::spawn(move || {
        let mut repository = match DuckdbCanFrameRepository::open(&database_path) {
            Ok(repository) => repository,
            Err(error) => {
                tracing::error!("automatic AI training could not open storage: {error}");
                return;
            }
        };
        repository.select_vehicle(vehicle_id);
        let generation = format!(
            "vehicle-{vehicle_id}-{}",
            chrono::Utc::now().timestamp_millis()
        );
        let request_id = format!("train-{generation}");
        if !repository
            .try_start_training_job(&request_id, &generation)
            .unwrap_or(false)
        {
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            let mut payload = repository.training_payload(&split, &schema)?;
            payload["model_id"] = serde_json::Value::String(generation.clone());
            let (mut worker, _) = Worker::spawn_discovered(&data_dir, Duration::from_secs(30))?;
            let response = worker.request("train", payload, Duration::from_secs(1800))?;
            anyhow::ensure!(
                response.ok,
                "{}",
                response.error.unwrap_or_else(|| "training failed".into())
            );
            let metadata = response.payload;
            let accepted =
                metadata.get("state").and_then(|value| value.as_str()) == Some("accepted");
            let reasons = metadata
                .get("decision_reasons")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<Vec<_>>();
            let artifact_path = metadata
                .get("artifact_path")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("training artifact path missing"))?;
            let artifact_sha256 = metadata
                .get("artifact_sha256")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("training artifact hash missing"))?;
            let scope = format!("vehicle-{vehicle_id}");
            repository.register_model_generation(&ModelGenerationRecord {
                generation: &generation,
                parent: None,
                schema: &schema,
                artifact_path,
                artifact_sha256,
                metrics: &metadata,
                accepted,
                reasons: &reasons,
                scope: &scope,
            })?;
            if accepted {
                let activation = worker.request(
                    "activate_model",
                    serde_json::json!({
                        "artifact_path":artifact_path,"artifact_sha256":artifact_sha256,
                        "feature_schema":&schema,"scope":&scope,
                    }),
                    Duration::from_secs(30),
                )?;
                anyhow::ensure!(
                    activation.ok,
                    "{}",
                    activation
                        .error
                        .unwrap_or_else(|| "model activation failed".into())
                );
                repository.activate_model_generation(&scope, &generation)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                let _ = repository.finish_training_job(&request_id, "completed", None);
            }
            Err(error) => {
                tracing::error!("automatic AI training failed: {error}");
                let _ =
                    repository.finish_training_job(&request_id, "failed", Some(&error.to_string()));
            }
        }
    });
}

#[derive(Debug, Clone)]
pub enum RealtimeLoggingEvent {
    Saved {
        total_frames: u64,
    },
    Decoded {
        name: String,
        value: f64,
        unit: Option<String>,
    },
    ReceiveError(String),
    ConnectionLost(String),
    VehicleChanged,
    ScanProgress(PidScanProgress),
    ScanFinished(PidScanProgress),
    SaveError(String),
    Stopped,
}

pub struct RealtimeLoggingSession {
    stop_requested: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    commands: Sender<LoggingCommand>,
}

enum LoggingCommand {
    StartScan(PidScanConfig),
    CancelScan,
}

pub struct RealtimeLoggingConfig {
    pub signal_kind: SignalKind,
    pub definitions: SignalDefinitionMap,
    pub log_database_path: PathBuf,
    pub master_database_path: PathBuf,
    pub vehicle_id: i64,
    pub connection_session_id: i64,
    pub expected_vin: Option<String>,
    pub realtime_state: Arc<RealtimeState>,
    pub events: Sender<RealtimeLoggingEvent>,
}

impl RealtimeLoggingSession {
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Relaxed);
    }
    pub fn start_pid_scan(&self, config: PidScanConfig) {
        let _ = self.commands.send(LoggingCommand::StartScan(config));
    }
    pub fn cancel_pid_scan(&self) {
        let _ = self.commands.send(LoggingCommand::CancelScan);
    }
}

impl Drop for RealtimeLoggingSession {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.handle.take();
    }
}

pub fn spawn_realtime_logging(
    source: Box<dyn CanFrameSource>,
    config: RealtimeLoggingConfig,
) -> RealtimeLoggingSession {
    let stop_requested = Arc::new(AtomicBool::new(false));
    let thread_stop_requested = stop_requested.clone();
    let (command_sender, command_receiver) = unbounded();

    let handle = thread::spawn(move || {
        run_realtime_logging(source, config, thread_stop_requested, command_receiver);
    });

    RealtimeLoggingSession {
        stop_requested,
        handle: Some(handle),
        commands: command_sender,
    }
}

fn run_realtime_logging(
    mut source: Box<dyn CanFrameSource>,
    config: RealtimeLoggingConfig,
    stop_requested: Arc<AtomicBool>,
    commands: Receiver<LoggingCommand>,
) {
    let RealtimeLoggingConfig {
        signal_kind,
        definitions,
        log_database_path,
        master_database_path,
        vehicle_id,
        connection_session_id,
        expected_vin,
        realtime_state,
        events,
    } = config;
    let mut repository = match DuckdbCanFrameRepository::open(&log_database_path) {
        Ok(repository) => repository,
        Err(error) => {
            let _ = events.send(RealtimeLoggingEvent::SaveError(
                format_duckdb_write_open_error(&log_database_path, &error).to_string(),
            ));
            let _ = events.send(RealtimeLoggingEvent::Stopped);
            return;
        }
    };
    repository.set_capture_context(vehicle_id, connection_session_id);
    let schema_version = format!("vehicle-{vehicle_id}-ai-window-v1");
    let ai = repository.active_ai_contract().ok().flatten();
    let saved_training_contract = repository
        .ai_feature_contract(&schema_version)
        .ok()
        .flatten();
    let training_contract =
        saved_training_contract.or_else(|| ai.as_ref().map(|(_, contract)| contract.clone()));
    let mut ai_collector = ai
        .as_ref()
        .map(|(_, contract)| RealtimeAiCollector::new(contract.clone()));
    let ai_runtime = ai.map(|(model_id, contract)| {
        let data_dir = log_database_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("ai-runtime");
        RealtimeAiRuntime::spawn(
            data_dir,
            model_id,
            format!("vehicle-{vehicle_id}"),
            contract,
        )
    });
    let mut ai_windows = Vec::<AiWindowResult>::new();
    let mut training_samples = Vec::<RawSignalSample>::new();
    let mut training_last_second = HashMap::<String, i64>::new();
    let mut automatic_training = None::<(SessionSplit, String)>;
    let master = SqliteMasterRepository::open(&master_database_path).ok();

    let mut buffer = Vec::with_capacity(SAVE_BATCH_SIZE);
    let mut last_flush = Instant::now();
    let mut last_receive_error = Instant::now() - RECEIVE_ERROR_INTERVAL;
    let mut total_frames = 0_u64;
    let mut consecutive_receive_errors = 0_u16;
    let mut last_identity_check = Instant::now();

    while !stop_requested.load(Ordering::Relaxed) {
        if last_identity_check.elapsed() >= Duration::from_secs(30) {
            if let (Some(expected), Ok(Some(observed))) =
                (expected_vin.as_deref(), source.vehicle_vin())
                && car_logger_application::connection::normalize_vin(&observed)
                    .ok()
                    .flatten()
                    .as_deref()
                    != Some(expected)
            {
                let _ = events.send(RealtimeLoggingEvent::VehicleChanged);
                break;
            }
            last_identity_check = Instant::now();
        }
        if let Ok(LoggingCommand::StartScan(config)) = commands.try_recv() {
            let progress = run_active_scan(
                &mut *source,
                config,
                &commands,
                master.as_ref(),
                vehicle_id,
                &events,
            );
            let _ = events.send(RealtimeLoggingEvent::ScanFinished(progress));
            continue;
        }
        match source.receive() {
            Ok(frame) => {
                consecutive_receive_errors = 0;
                if signal_kind == SignalKind::CanId
                    && let Some(master) = &master
                    && let Err(error) = master.observe_can_id(
                        vehicle_id,
                        frame.id,
                        frame.is_extended,
                        frame.data.len() as u8,
                        frame.received_at,
                    )
                {
                    let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
                        "CAN ID observation was not saved: {error}"
                    )));
                }
                if let Some(decoded_values) = decode_frame(signal_kind, &frame, &definitions) {
                    for decoded_value in &decoded_values {
                        let _ = events.send(RealtimeLoggingEvent::Decoded {
                            name: decoded_value.name.clone(),
                            value: decoded_value.value,
                            unit: decoded_value.unit.clone(),
                        });
                        if let Some(collector) = &mut ai_collector {
                            let slow = decoded_value
                                .name
                                .to_ascii_lowercase()
                                .contains("temperature");
                            collector.observe(
                                &decoded_value.name,
                                decoded_value.value,
                                frame.received_at,
                                slow,
                            );
                        }
                        let key = canonical_signal_key(&decoded_value.name);
                        let second = frame.received_at.timestamp();
                        if decoded_value.value.is_finite()
                            && training_last_second.get(&key) != Some(&second)
                        {
                            training_last_second.insert(key.clone(), second);
                            training_samples.push(RawSignalSample {
                                key,
                                value: decoded_value.value,
                                at: frame.received_at,
                                slow: decoded_value
                                    .name
                                    .to_ascii_lowercase()
                                    .contains("temperature"),
                            });
                        }
                    }
                    if let (Some(collector), Some(runtime)) = (&mut ai_collector, &ai_runtime)
                        && let Some(window) = collector.take_due(frame.received_at)
                    {
                        let _ = runtime.windows.try_send(window);
                    }
                    realtime_state.upsert_known(frame.clone(), decoded_values);
                } else {
                    realtime_state.upsert_unknown(frame.clone());
                }
                buffer.push(frame);
                if let Some(runtime) = &ai_runtime {
                    while let Ok(result) = runtime.results.try_recv() {
                        match result {
                            Ok(result) if result.availability == AiAvailability::Available => {
                                if let Err(error) = repository
                                    .save_ai_inference_result(&result, Some(connection_session_id))
                                {
                                    let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
                                        "AI result was not saved: {error}"
                                    )));
                                } else {
                                    ai_windows.push(result);
                                    let evaluation = evaluate_session(&ai_windows);
                                    if let (Some(first), Some(last)) =
                                        (ai_windows.first(), ai_windows.last())
                                    {
                                        let _ = repository.save_session_ai_evaluation(
                                            "session",
                                            first.window_start,
                                            last.window_start + chrono::Duration::seconds(60),
                                            &evaluation,
                                        );
                                        let statistical = repository
                                            .latest_score(ScoreGranularity::Day)
                                            .ok()
                                            .flatten()
                                            .and_then(|score| score.score);
                                        let maturity =
                                            repository.ai_model_maturity().unwrap_or(0.0);
                                        let overall = overall_condition(
                                            statistical,
                                            evaluation.score,
                                            evaluation.confidence,
                                            maturity,
                                        );
                                        let _ = repository.save_overall_condition(
                                            &OverallConditionRecord {
                                                granularity: "session",
                                                start: first.window_start,
                                                end: last.window_start
                                                    + chrono::Duration::seconds(60),
                                                statistical_score: statistical,
                                                ai_score: evaluation.score,
                                                ai_confidence: evaluation.confidence,
                                                model_maturity: maturity,
                                                condition: &overall,
                                            },
                                        );
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(error) => {
                                let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
                                    "AI is unavailable; logging continues: {error}"
                                )));
                            }
                        }
                    }
                }
                while let Some(observation) = source.take_diagnostic_observation() {
                    if let Err(error) = repository.record_diagnostic(&observation) {
                        let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
                            "Diagnostic observation was not saved: {error}"
                        )));
                    }
                }

                if buffer.len() >= SAVE_BATCH_SIZE || last_flush.elapsed() >= FLUSH_INTERVAL {
                    flush_buffer(
                        &mut repository,
                        signal_kind,
                        &mut buffer,
                        &events,
                        &mut total_frames,
                    );
                    last_flush = Instant::now();
                }
            }
            Err(error) => {
                consecutive_receive_errors = consecutive_receive_errors.saturating_add(1);
                if last_receive_error.elapsed() >= RECEIVE_ERROR_INTERVAL {
                    let _ = events.send(RealtimeLoggingEvent::ReceiveError(error.to_string()));
                    last_receive_error = Instant::now();
                }
                thread::sleep(Duration::from_millis(20));
                if consecutive_receive_errors >= 50 {
                    let _ = events.send(RealtimeLoggingEvent::ConnectionLost(error.to_string()));
                    break;
                }
            }
        }

        if buffer.len() > MAX_BUFFERED_FRAMES {
            let dropped = buffer.len() - MAX_BUFFERED_FRAMES;
            buffer.drain(..dropped);
            let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
                "Dropped {dropped} buffered frames after repeated save failures"
            )));
        }
    }

    if !training_samples.is_empty() {
        let contract = training_contract
            .or_else(|| feature_contract(&training_samples, schema_version.clone()));
        if let Some(contract) = contract {
            if repository
                .ai_feature_contract(&contract.schema_version)
                .ok()
                .flatten()
                .is_none()
            {
                let signals = contract
                    .signal_keys
                    .iter()
                    .filter_map(|key| {
                        contract
                            .normalization
                            .get(key)
                            .map(|normalization| (key.clone(), normalization.clone(), 1.0))
                    })
                    .collect::<Vec<_>>();
                if let Err(error) = repository.save_ai_schema(&contract.schema_version, &signals) {
                    let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
                        "AI feature contract was not saved: {error}"
                    )));
                }
            }
            for window in build_windows_with_contract(&training_samples, true, &contract) {
                if let Err(error) = repository.save_ai_window(
                    &window,
                    Some(connection_session_id),
                    "training",
                    true,
                ) {
                    let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
                        "AI training window was not saved: {error}"
                    )));
                    break;
                }
            }
            if let Ok(TrainingReadiness::Ready(split)) =
                repository.refresh_ai_training_readiness(chrono::Utc::now())
                && repository
                    .automatic_training_due(chrono::Utc::now())
                    .unwrap_or(false)
            {
                automatic_training = Some((split, contract.schema_version.clone()));
            }
        }
    }

    if let Some(observation) = source.final_diagnostic_observation()
        && let Err(error) = repository.record_diagnostic(&observation)
    {
        let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
            "Final diagnostic observation was not saved: {error}"
        )));
    }
    flush_buffer(
        &mut repository,
        signal_kind,
        &mut buffer,
        &events,
        &mut total_frames,
    );
    if let Some(master) = master
        && let Err(error) = master.end_connection_session(
            connection_session_id,
            chrono::Utc::now(),
            "capture_stopped",
        )
    {
        let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
            "Connection session was not closed: {error}"
        )));
    }
    drop(repository);
    if let Some((split, schema)) = automatic_training {
        let data_dir = log_database_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("ai-runtime");
        spawn_automatic_training(log_database_path, data_dir, vehicle_id, split, schema);
    }
    let _ = events.send(RealtimeLoggingEvent::Stopped);
}

fn run_active_scan(
    source: &mut dyn CanFrameSource,
    config: PidScanConfig,
    commands: &Receiver<LoggingCommand>,
    master: Option<&SqliteMasterRepository>,
    vehicle_id: i64,
    events: &Sender<RealtimeLoggingEvent>,
) -> PidScanProgress {
    if config.validate().is_err() {
        return PidScanProgress {
            stopped: true,
            errors: 1,
            ..Default::default()
        };
    }
    let mut progress = PidScanProgress::default();
    let history_id = master.and_then(|master| {
        master
            .start_pid_scan(
                vehicle_id,
                config.service,
                config.start_pid,
                config.end_pid,
                config.interval.as_millis() as u64,
                chrono::Utc::now(),
            )
            .ok()
    });
    let mut consecutive_errors = 0_u8;
    for pid in config.start_pid..=config.end_pid {
        if matches!(commands.try_recv(), Ok(LoggingCommand::CancelScan)) {
            progress.stopped = true;
            break;
        }
        match source.probe_pid(config.service, pid, config.response_timeout) {
            Ok(responded) => {
                consecutive_errors = 0;
                if responded {
                    progress.responses += 1;
                    if let Some(master) = master {
                        let _ = master.observe_unknown_pid(
                            vehicle_id,
                            "",
                            config.service,
                            pid,
                            chrono::Utc::now(),
                        );
                    }
                }
            }
            Err(_) => {
                progress.errors += 1;
                consecutive_errors += 1;
                if consecutive_errors >= 5 {
                    progress.stopped = true;
                }
            }
        }
        progress.scanned += 1;
        let _ = events.send(RealtimeLoggingEvent::ScanProgress(progress));
        if progress.stopped {
            break;
        }
        thread::sleep(config.interval);
    }
    if let (Some(master), Some(id)) = (master, history_id) {
        let status = if progress.stopped {
            "stopped"
        } else {
            "completed"
        };
        let _ = master.finish_pid_scan(
            id,
            progress.scanned,
            progress.responses,
            progress.errors,
            status,
            chrono::Utc::now(),
        );
    }
    progress
}

fn format_duckdb_write_open_error(path: &std::path::Path, error: &anyhow::Error) -> String {
    format!(
        "DuckDB log is not writable: {}. Close read-only viewers such as RustRover database tools, then connect again. Detail: {error}",
        path.display()
    )
}

fn flush_buffer(
    repository: &mut DuckdbCanFrameRepository,
    signal_kind: SignalKind,
    buffer: &mut Vec<CanFrame>,
    events: &Sender<RealtimeLoggingEvent>,
    total_frames: &mut u64,
) {
    if buffer.is_empty() {
        return;
    }

    match repository.save_batch_with_kind(signal_kind, buffer) {
        Ok(()) => {
            *total_frames += buffer.len() as u64;
            buffer.clear();
            let _ = events.send(RealtimeLoggingEvent::Saved {
                total_frames: *total_frames,
            });
        }
        Err(error) => {
            let _ = events.send(RealtimeLoggingEvent::SaveError(error.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_payload_is_converted_to_a_complete_window_result() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let response = Response {
            request_id: "worker-request".into(),
            protocol_version: 1,
            kind: "infer".into(),
            ok: true,
            payload: serde_json::json!({
                "reconstruction_error":1.0,"anomaly":0.5,"score":80.0,
                "confidence":0.9,"coverage":0.8,"model_id":"m1",
                "feature_schema":"s1","driving_state":"steady_cruise",
                "contributions":[{
                    "signal_name":"rpm","driving_state":"steady_cruise",
                    "window_start":"2026-01-01T00:00:00Z","rank":1,
                    "reconstruction_error":1.0,"normal_median":0.5,
                    "normal_p95":1.5,"normal_p99":2.0,"percentile":75.0,
                    "consecutive_count":1,"coverage":0.8
                }]
            }),
            error: None,
        };
        let result = parse_inference_response(response, "app-request".into(), at).unwrap();
        assert_eq!(result.request_id, "app-request");
        assert_eq!(result.availability, AiAvailability::Available);
        assert_eq!(result.contributions[0].rank, 1);
    }

    #[test]
    fn failed_worker_response_does_not_create_a_score() {
        let response = Response {
            request_id: "r".into(),
            protocol_version: 1,
            kind: "infer".into(),
            ok: false,
            payload: serde_json::json!({}),
            error: Some("model mismatch".into()),
        };
        assert!(parse_inference_response(response, "r".into(), chrono::Utc::now()).is_err());
    }
}
