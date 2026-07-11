use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use car_logger_application::CanFrameSource;
use car_logger_domain::{CanFrame, RealtimeState, SignalKind};
use car_logger_storage::DuckdbCanFrameRepository;
use crossbeam_channel::Sender;

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
    SaveError(String),
    Stopped,
}

pub struct RealtimeLoggingSession {
    stop_requested: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RealtimeLoggingSession {
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Relaxed);
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
    signal_kind: SignalKind,
    definitions: SignalDefinitionMap,
    log_database_path: PathBuf,
    realtime_state: Arc<RealtimeState>,
    events: Sender<RealtimeLoggingEvent>,
) -> RealtimeLoggingSession {
    let stop_requested = Arc::new(AtomicBool::new(false));
    let thread_stop_requested = stop_requested.clone();

    let handle = thread::spawn(move || {
        run_realtime_logging(
            source,
            signal_kind,
            definitions,
            log_database_path,
            realtime_state,
            events,
            thread_stop_requested,
        );
    });

    RealtimeLoggingSession {
        stop_requested,
        handle: Some(handle),
    }
}

fn run_realtime_logging(
    mut source: Box<dyn CanFrameSource>,
    signal_kind: SignalKind,
    definitions: SignalDefinitionMap,
    log_database_path: PathBuf,
    realtime_state: Arc<RealtimeState>,
    events: Sender<RealtimeLoggingEvent>,
    stop_requested: Arc<AtomicBool>,
) {
    let mut repository = match DuckdbCanFrameRepository::open(&log_database_path) {
        Ok(repository) => repository,
        Err(error) => {
            let _ = events.send(RealtimeLoggingEvent::SaveError(format!(
                "{}",
                format_duckdb_write_open_error(&log_database_path, &error)
            )));
            let _ = events.send(RealtimeLoggingEvent::Stopped);
            return;
        }
    };

    let mut buffer = Vec::with_capacity(SAVE_BATCH_SIZE);
    let mut last_flush = Instant::now();
    let mut last_receive_error = Instant::now() - RECEIVE_ERROR_INTERVAL;
    let mut total_frames = 0_u64;

    while !stop_requested.load(Ordering::Relaxed) {
        match source.receive() {
            Ok(frame) => {
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
                if last_receive_error.elapsed() >= RECEIVE_ERROR_INTERVAL {
                    let _ = events.send(RealtimeLoggingEvent::ReceiveError(error.to_string()));
                    last_receive_error = Instant::now();
                }
                thread::sleep(Duration::from_millis(20));
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

    flush_buffer(
        &mut repository,
        signal_kind,
        &mut buffer,
        &events,
        &mut total_frames,
    );
    let _ = events.send(RealtimeLoggingEvent::Stopped);
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
