use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use car_logger_application::pid_scan::{PidScanConfig, PidScanProgress};
use car_logger_application::{CanFrameSource, DiagnosticRepository};
use car_logger_domain::{CanFrame, RealtimeState, SignalKind};
use car_logger_storage::{DuckdbCanFrameRepository, SqliteMasterRepository};
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::signal_decoder::{SignalDefinitionMap, decode_frame};

const FLUSH_INTERVAL: Duration = Duration::from_millis(500);
const RECEIVE_ERROR_INTERVAL: Duration = Duration::from_secs(2);
const SAVE_BATCH_SIZE: usize = 128;
const MAX_BUFFERED_FRAMES: usize = 10_000;

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
                    }
                    realtime_state.upsert_known(frame.clone(), decoded_values);
                } else {
                    realtime_state.upsert_unknown(frame.clone());
                }
                buffer.push(frame);
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
