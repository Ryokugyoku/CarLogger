use anyhow::Result;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

pub const SAFE_READ_SERVICES: [u8; 3] = [0x01, 0x02, 0x09];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidScanConfig {
    pub service: u8,
    pub start_pid: u8,
    pub end_pid: u8,
    pub interval: Duration,
    pub response_timeout: Duration,
}

impl Default for PidScanConfig {
    fn default() -> Self {
        Self {
            service: 0x01,
            start_pid: 0,
            end_pid: 0x20,
            interval: Duration::from_millis(100),
            response_timeout: Duration::from_secs(1),
        }
    }
}

impl PidScanConfig {
    pub fn validate(self) -> Result<()> {
        anyhow::ensure!(
            SAFE_READ_SERVICES.contains(&self.service),
            "読み取り専用OBDサービスだけを探索できます"
        );
        anyhow::ensure!(self.start_pid <= self.end_pid, "PID範囲が不正です");
        anyhow::ensure!(
            self.interval >= Duration::from_millis(20),
            "送信間隔は20ms以上が必要です"
        );
        anyhow::ensure!(
            !self.response_timeout.is_zero(),
            "応答タイムアウトが必要です"
        );
        Ok(())
    }
}

pub trait PidProbe {
    fn is_connected(&self) -> bool;
    fn probe(&mut self, service: u8, pid: u8, timeout: Duration) -> Result<bool>;
}

pub trait ScanSleeper {
    fn sleep(&mut self, duration: Duration);
}
pub struct ThreadSleeper;
impl ScanSleeper for ThreadSleeper {
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PidScanProgress {
    pub scanned: u16,
    pub responses: u16,
    pub errors: u16,
    pub stopped: bool,
}

#[derive(Clone, Default)]
pub struct ScanCancellation(Arc<AtomicBool>);
impl ScanCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

pub fn run_scan<P: PidProbe, S: ScanSleeper>(
    probe: &mut P,
    sleeper: &mut S,
    config: PidScanConfig,
    cancellation: &ScanCancellation,
    mut progress: impl FnMut(PidScanProgress),
) -> Result<PidScanProgress> {
    config.validate()?;
    let mut state = PidScanProgress::default();
    let mut consecutive_errors = 0_u8;
    for pid in config.start_pid..=config.end_pid {
        if cancellation.cancelled() || !probe.is_connected() {
            state.stopped = true;
            break;
        }
        match probe.probe(config.service, pid, config.response_timeout) {
            Ok(responded) => {
                state.responses += u16::from(responded);
                consecutive_errors = 0;
            }
            Err(_) => {
                state.errors += 1;
                consecutive_errors += 1;
                if consecutive_errors >= 5 {
                    state.stopped = true;
                    state.scanned += 1;
                    progress(state);
                    break;
                }
            }
        }
        state.scanned += 1;
        progress(state);
        if pid != config.end_pid {
            sleeper.sleep(config.interval);
        }
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        connected: bool,
        sent: Vec<(u8, u8)>,
        fail: bool,
    }
    impl PidProbe for Fake {
        fn is_connected(&self) -> bool {
            self.connected
        }
        fn probe(&mut self, s: u8, p: u8, _: Duration) -> Result<bool> {
            self.sent.push((s, p));
            if self.fail {
                anyhow::bail!("timeout")
            }
            Ok(p.is_multiple_of(2))
        }
    }
    struct NoSleep;
    impl ScanSleeper for NoSleep {
        fn sleep(&mut self, _: Duration) {}
    }
    #[test]
    fn only_read_services_are_allowed() {
        let mut f = Fake {
            connected: true,
            sent: vec![],
            fail: false,
        };
        assert!(
            run_scan(
                &mut f,
                &mut NoSleep,
                PidScanConfig {
                    service: 4,
                    ..Default::default()
                },
                &ScanCancellation::default(),
                |_| {}
            )
            .is_err()
        );
        assert!(f.sent.is_empty());
    }
    #[test]
    fn scan_reports_progress_and_can_be_cancelled() {
        let mut f = Fake {
            connected: true,
            sent: vec![],
            fail: false,
        };
        let cancel = ScanCancellation::default();
        let c = cancel.clone();
        let result = run_scan(
            &mut f,
            &mut NoSleep,
            PidScanConfig {
                end_pid: 10,
                ..Default::default()
            },
            &cancel,
            |p| {
                if p.scanned == 3 {
                    c.cancel()
                }
            },
        )
        .unwrap();
        assert_eq!(result.scanned, 3);
        assert!(result.stopped);
    }
    #[test]
    fn five_consecutive_errors_stop_the_scan() {
        let mut f = Fake {
            connected: true,
            sent: vec![],
            fail: true,
        };
        let result = run_scan(
            &mut f,
            &mut NoSleep,
            Default::default(),
            &ScanCancellation::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.errors, 5);
        assert!(result.stopped);
    }
}
