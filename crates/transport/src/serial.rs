use std::{
    collections::{BTreeSet, VecDeque},
    io::{ErrorKind, Read, Write},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use car_logger_application::{
    CanFrameSource, DiagnosticObservation, DiagnosticQuality, DtcReading,
};
use car_logger_domain::CanFrame;
use serialport::{
    ClearBuffer, DataBits, FlowControl, Parity, SerialPort, SerialPortType, StopBits,
};

use crate::{ConnectedInterface, ConnectionMode};

pub struct SerialCanSource {
    port: Box<dyn SerialPort>,
    parser: SerialFrameParser,
    obd_poll_pids: Vec<u8>,
    obd_poll_plan: Vec<Vec<u8>>,
    obd_poll_plan_index: usize,
    obd_multi_pid_enabled: bool,
    diagnostic: DiagnosticScheduler,
}

#[derive(Debug, Clone, Copy)]
pub struct DiagnosticPollingConfig {
    pub interval: Duration,
    pub normal_requests_between_diagnostics: u8,
    pub request_timeout: Duration,
}

impl Default for DiagnosticPollingConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(5 * 60),
            normal_requests_between_diagnostics: 4,
            request_timeout: Duration::from_millis(2_500),
        }
    }
}

struct DiagnosticScheduler {
    config: DiagnosticPollingConfig,
    next_due: Instant,
    normal_requests: u8,
    stage: u8,
    status: Option<(bool, u8)>,
    unsupported: bool,
    pending: VecDeque<DiagnosticObservation>,
}

impl DiagnosticScheduler {
    fn new(config: DiagnosticPollingConfig) -> Self {
        Self {
            config,
            next_due: Instant::now(),
            normal_requests: 0,
            stage: 0,
            status: None,
            unsupported: false,
            pending: VecDeque::new(),
        }
    }

    fn note_normal_request(&mut self) {
        self.normal_requests = self.normal_requests.saturating_add(1);
    }

    fn ready(&self) -> bool {
        !self.unsupported
            && Instant::now() >= self.next_due
            && self.normal_requests >= self.config.normal_requests_between_diagnostics
    }

    fn consume_turn(&mut self) {
        self.normal_requests = 0;
    }
}

const DASHBOARD_OBD_PIDS: &[u8] = &[0x0C, 0x0D, 0x05, 0x11];
// Keep this list aligned with the built-in Mode 01 definitions in
// crates/storage/src/builtin_signals.rs. Some ECUs answer a PID even when the
// supported-PID bitmap does not advertise it, so every built-in PID is polled.
const STANDARD_OBD_POLL_PIDS: &[u8] = &[
    0x04, 0x05, 0x06, 0x07, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x1F, 0x21, 0x2F, 0x31, 0x33,
    0x3C, 0x42, 0x43, 0x45, 0x46, 0x47, 0x49, 0x4A, 0x4C, 0x5C,
];
const MODE_01_SUPPORTED_PID_QUERY_PIDS: &[u8] = &[0x00, 0x20, 0x40, 0x60, 0x80, 0xA0, 0xC0, 0xE0];
const FAST_OBD_PIDS: &[u8] = &[0x0C, 0x0D, 0x04, 0x0B, 0x10, 0x11];
const MEDIUM_OBD_PIDS: &[u8] = &[0x06, 0x07, 0x0E, 0x42, 0x43, 0x45, 0x47, 0x49, 0x4A, 0x4C];
const SLOW_OBD_PIDS: &[u8] = &[0x05, 0x0F, 0x1F, 0x21, 0x2F, 0x31, 0x33, 0x3C, 0x46, 0x5C];

pub fn list_serial_interfaces() -> Vec<ConnectedInterface> {
    serialport::available_ports()
        .map(|ports| {
            ports
                .into_iter()
                .map(|port| {
                    let manufacturer = match port.port_type {
                        SerialPortType::UsbPort(info) => info
                            .manufacturer
                            .unwrap_or_else(|| "Unknown USB".to_string()),
                        SerialPortType::BluetoothPort => "Bluetooth".to_string(),
                        SerialPortType::PciPort => "PCI serial".to_string(),
                        SerialPortType::Unknown => "Unknown".to_string(),
                    };

                    ConnectedInterface {
                        name: port.port_name.clone(),
                        manufacturer,
                        path: port.port_name,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

impl SerialCanSource {
    pub fn open(port_name: &str, baud_rate: u32) -> Result<Self> {
        Self::open_with_mode(port_name, baud_rate, ConnectionMode::Stream)
    }

    pub fn open_obd2_auto(port_name: &str) -> Result<Self> {
        const CANDIDATE_BAUD_RATES: &[u32] = &[115_200, 38_400, 57_600, 9_600];

        let mut failures = Vec::new();
        for baud_rate in CANDIDATE_BAUD_RATES {
            match Self::open_obd2_with_probe(port_name, *baud_rate) {
                Ok(source) => {
                    tracing::info!(port = port_name, baud_rate, "ELM327 OBD-II probe succeeded");
                    return Ok(source);
                }
                Err(error) => {
                    tracing::warn!(
                        port = port_name,
                        baud_rate,
                        "ELM327 OBD-II probe failed: {error}"
                    );
                    failures.push(format!("{baud_rate}: {error}"));
                }
            }
        }

        anyhow::bail!(
            "ELM327からOBD-II応答を取得できませんでした。試行ボーレート: {}",
            failures.join(", ")
        )
    }

    pub fn open_with_mode(port_name: &str, baud_rate: u32, mode: ConnectionMode) -> Result<Self> {
        let mut port = serialport::new(port_name, baud_rate)
            .data_bits(DataBits::Eight)
            .flow_control(FlowControl::None)
            .parity(Parity::None)
            .stop_bits(StopBits::One)
            .timeout(Duration::from_millis(250))
            .open()
            .with_context(|| format!("シリアルポートを開けませんでした: {port_name}"))?;

        if mode == ConnectionMode::Obd2 {
            initialize_elm327(&mut port)?;
        }

        Ok(Self {
            port,
            parser: SerialFrameParser::new(mode),
            obd_poll_pids: standard_obd_poll_pids(),
            obd_poll_plan: Vec::new(),
            obd_poll_plan_index: 0,
            obd_multi_pid_enabled: true,
            diagnostic: DiagnosticScheduler::new(DiagnosticPollingConfig::default()),
        })
    }

    fn open_obd2_with_probe(port_name: &str, baud_rate: u32) -> Result<Self> {
        let mut source = Self::open_with_mode(port_name, baud_rate, ConnectionMode::Obd2)?;
        source.configure_obd_poll_pids()?;
        let frame = source.probe_any_obd_pid(Duration::from_millis(8_000))?;
        source.parser.frames.push_back(frame);
        Ok(source)
    }
}

impl CanFrameSource for SerialCanSource {
    fn receive(&mut self) -> Result<CanFrame> {
        loop {
            if let Some(frame) = self.parser.next_frame() {
                return Ok(frame);
            }

            if self.parser.mode == ConnectionMode::Obd2 {
                self.poll_next_obd_group()?;
                self.maybe_poll_diagnostics();
                continue;
            }

            let mut buffer = [0_u8; 256];
            let read_size = self
                .port
                .read(&mut buffer)
                .context("シリアルデータの読み取りに失敗しました")?;
            self.parser.push_bytes(&buffer[..read_size]);
        }
    }

    fn vehicle_vin(&mut self) -> Result<Option<String>> {
        if self.parser.mode != ConnectionMode::Obd2 {
            return Ok(None);
        }
        write_elm327_command(&mut self.port, "0902")?;
        let response = read_elm327_response(&mut self.port, Duration::from_millis(4_000));
        Ok(parse_mode_09_vin(&response))
    }

    fn take_diagnostic_observation(&mut self) -> Option<DiagnosticObservation> {
        self.diagnostic.pending.pop_front()
    }

    fn final_diagnostic_observation(&mut self) -> Option<DiagnosticObservation> {
        if self.parser.mode != ConnectionMode::Obd2 || self.diagnostic.unsupported {
            return None;
        }
        let status_response = self.send_diagnostic_command("0101");
        let status = parse_monitor_status(&status_response);
        let dtc_response = self.send_diagnostic_command("03");
        let dtcs = parse_mode_03_dtcs(&dtc_response);
        let (mil_on, count) = status.as_ref().copied().unwrap_or((false, 0));
        let error = status.err().or_else(|| dtcs.as_ref().err().cloned());
        Some(DiagnosticObservation {
            observed_at: chrono::Utc::now(),
            mil_on: error.is_none().then_some(mil_on),
            reported_dtc_count: error.is_none().then_some(count),
            dtcs: dtcs.unwrap_or_default(),
            source_service: "obd2_session_end".into(),
            quality: if error.is_some() {
                DiagnosticQuality::Failed
            } else {
                DiagnosticQuality::Complete
            },
            error,
            session_id: None,
        })
    }
}

fn parse_mode_09_vin(response: &str) -> Option<String> {
    let mut collecting = false;
    let mut bytes = Vec::new();
    for line in elm327_response_lines(response) {
        let mut line_bytes = line
            .split_whitespace()
            .filter_map(|token| {
                let token = token.trim_matches(|c: char| !c.is_ascii_hexdigit());
                (token.len() == 2)
                    .then(|| u8::from_str_radix(token, 16).ok())
                    .flatten()
            })
            .collect::<Vec<_>>();
        if let Some(start) = line_bytes.windows(3).position(|x| x == [0x49, 0x02, 0x01]) {
            collecting = true;
            line_bytes.drain(..start + 3);
        } else if collecting
            && line_bytes
                .first()
                .is_some_and(|x| (0x20..=0x2f).contains(x))
        {
            line_bytes.remove(0);
        } else if !collecting {
            continue;
        }
        bytes.extend(line_bytes.into_iter().filter(|x| x.is_ascii_alphanumeric()));
    }
    let vin = String::from_utf8(bytes.into_iter().take(17).collect()).ok()?;
    (vin.len() == 17).then(|| vin.to_ascii_uppercase())
}

impl SerialCanSource {
    fn poll_next_obd_group(&mut self) -> Result<()> {
        anyhow::ensure!(
            !self.obd_poll_pids.is_empty(),
            "ポーリング可能なOBD-II Mode 01 PIDがありません"
        );

        if self.obd_poll_plan.is_empty() {
            self.obd_poll_plan = build_obd_poll_plan(&self.obd_poll_pids);
        }
        self.obd_poll_plan_index %= self.obd_poll_plan.len();
        let mut pids = self.obd_poll_plan[self.obd_poll_plan_index].clone();
        self.obd_poll_plan_index = (self.obd_poll_plan_index + 1) % self.obd_poll_plan.len();
        if !self.obd_multi_pid_enabled {
            pids.truncate(1);
        }

        let command = format_obd_request(&pids);
        write_elm327_command(&mut self.port, &command)?;
        let response = read_elm327_response(&mut self.port, Duration::from_millis(2_500));
        self.diagnostic.note_normal_request();
        tracing::debug!(
            pids = %format_pid_list(&pids),
            response,
            "ELM327 poll response"
        );

        let frames = if pids.len() == 1 && mode_01_pid_data_len(pids[0]).is_none() {
            elm327_response_lines(&response)
                .filter_map(parse_elm327_obd_response)
                .filter(|frame| frame.id == u32::from(pids[0]))
                .collect()
        } else {
            parse_elm327_obd_response_for_pids(&response, &pids)
        };
        if !frames.is_empty() {
            self.parser.frames.extend(frames);
            return Ok(());
        }

        if pids.len() > 1 {
            self.obd_multi_pid_enabled = false;
            self.obd_poll_plan = build_single_pid_fallback_plan(&self.obd_poll_pids);
            self.obd_poll_plan_index = 0;
            tracing::warn!(
                response,
                "ELM327 multi-PID request failed; falling back to prioritized single-PID polling"
            );
            return Ok(());
        }

        if is_elm327_no_data_response(&response) {
            let pid = pids[0];
            self.obd_poll_pids.retain(|candidate| *candidate != pid);
            self.obd_poll_plan = build_single_pid_fallback_plan(&self.obd_poll_pids);
            self.obd_poll_plan_index = 0;
            tracing::info!(
                pid = format_args!("0x{pid:02X}"),
                "ELM327 PID returned NO DATA; disabling it for this connection"
            );
        }
        Ok(())
    }

    fn maybe_poll_diagnostics(&mut self) {
        if !self.diagnostic.ready() {
            return;
        }
        self.diagnostic.consume_turn();
        if self.diagnostic.stage == 0 {
            let response = self.send_diagnostic_command("0101");
            match parse_monitor_status(&response) {
                Ok(status) => {
                    self.diagnostic.status = Some(status);
                    self.diagnostic.stage = 1;
                }
                Err(error) => self.finish_diagnostic(Vec::new(), error),
            }
        } else {
            let response = self.send_diagnostic_command("03");
            match parse_mode_03_dtcs(&response) {
                Ok(dtcs) => self.finish_diagnostic(dtcs, String::new()),
                Err(error) => self.finish_diagnostic(Vec::new(), error),
            }
        }
    }

    fn send_diagnostic_command(&mut self, command: &str) -> String {
        if write_elm327_command(&mut self.port, command).is_err() {
            return "write failed".into();
        }
        read_elm327_response(&mut self.port, self.diagnostic.config.request_timeout)
    }

    fn finish_diagnostic(&mut self, dtcs: Vec<DtcReading>, error: String) {
        let no_data = error.eq_ignore_ascii_case("NO DATA");
        if no_data {
            self.diagnostic.unsupported = true;
        }
        let (mil_on, count) = self.diagnostic.status.unwrap_or((false, 0));
        let failed = !error.is_empty();
        self.diagnostic.pending.push_back(DiagnosticObservation {
            observed_at: chrono::Utc::now(),
            mil_on: self.diagnostic.status.map(|status| status.0),
            reported_dtc_count: self.diagnostic.status.map(|status| status.1),
            dtcs,
            source_service: "obd2_mode_01_01_and_03".into(),
            quality: if no_data {
                DiagnosticQuality::Unsupported
            } else if failed {
                DiagnosticQuality::Failed
            } else if usize::from(count) == 0 || mil_on || count > 0 {
                DiagnosticQuality::Complete
            } else {
                DiagnosticQuality::Partial
            },
            error: failed.then_some(error),
            session_id: None,
        });
        self.diagnostic.status = None;
        self.diagnostic.stage = 0;
        self.diagnostic.next_due = Instant::now() + self.diagnostic.config.interval;
    }

    fn configure_obd_poll_pids(&mut self) -> Result<()> {
        let supported = self.discover_supported_mode_01_pids()?;
        let poll_pids = obd_poll_pids(&supported);

        tracing::info!(
            supported_pids = %format_pid_set(&supported),
            poll_pids = %format_pid_list(&poll_pids),
            "ELM327 OBD-II PID support discovered"
        );
        self.obd_poll_pids = poll_pids;
        self.obd_poll_plan = build_obd_poll_plan(&self.obd_poll_pids);
        self.obd_poll_plan_index = 0;
        Ok(())
    }

    fn discover_supported_mode_01_pids(&mut self) -> Result<BTreeSet<u8>> {
        let mut supported = BTreeSet::new();
        for &base_pid in MODE_01_SUPPORTED_PID_QUERY_PIDS {
            if base_pid != 0x00 && !supported.contains(&base_pid) {
                continue;
            }

            let frame = self.probe_obd_pid(base_pid, Duration::from_millis(8_000))?;
            supported.extend(parse_supported_pid_bits(base_pid, &frame.data));
        }

        Ok(supported)
    }

    fn probe_any_obd_pid(&mut self, timeout: Duration) -> Result<CanFrame> {
        let started_at = Instant::now();
        let probe_pids = self.obd_poll_pids.clone();
        let mut failures = Vec::new();

        while started_at.elapsed() < timeout {
            for pid in &probe_pids {
                match self.probe_obd_pid(*pid, Duration::from_millis(2_500)) {
                    Ok(frame) => return Ok(frame),
                    Err(error) => failures.push(format!("0x{pid:02X}: {error}")),
                }
            }
        }

        anyhow::bail!(
            "対応PIDからOBD-II応答を取得できませんでした: {}",
            failures.join(", ")
        )
    }

    fn probe_obd_pid(&mut self, pid: u8, timeout: Duration) -> Result<CanFrame> {
        write_elm327_command(&mut self.port, &format!("01{pid:02X}"))?;
        let response = read_elm327_response(&mut self.port, timeout);
        tracing::debug!(
            pid = format_args!("0x{pid:02X}"),
            response,
            "ELM327 probe response"
        );

        for line in elm327_response_lines(&response) {
            if let Some(frame) = parse_elm327_obd_response(line)
                && frame.id == u32::from(pid)
            {
                return Ok(frame);
            }
        }

        if is_elm327_error_response(&response) {
            anyhow::bail!("PID 0x{pid:02X} がELM327/STNで失敗しました: {response:?}");
        }

        anyhow::bail!("PID 0x{pid:02X} の応答を解析できませんでした: {response:?}")
    }
}

fn standard_obd_poll_pids() -> Vec<u8> {
    let mut poll_pids = DASHBOARD_OBD_PIDS.to_vec();
    for pid in STANDARD_OBD_POLL_PIDS {
        if !poll_pids.contains(pid) {
            poll_pids.push(*pid);
        }
    }
    poll_pids
}

fn obd_poll_pids(supported: &BTreeSet<u8>) -> Vec<u8> {
    let mut poll_pids = standard_obd_poll_pids();

    // Also retain advertised Mode 01 PIDs that do not yet have a built-in
    // definition, so they can be observed and defined from the GUI.
    for pid in supported {
        if !MODE_01_SUPPORTED_PID_QUERY_PIDS.contains(pid) && !poll_pids.contains(pid) {
            poll_pids.push(*pid);
        }
    }

    poll_pids
}

fn build_obd_poll_plan(pids: &[u8]) -> Vec<Vec<u8>> {
    let fast = pid_batches(FAST_OBD_PIDS, pids);
    let medium = pid_batches(MEDIUM_OBD_PIDS, pids);
    let slow = pid_batches(SLOW_OBD_PIDS, pids);
    let classified = FAST_OBD_PIDS
        .iter()
        .chain(MEDIUM_OBD_PIDS)
        .chain(SLOW_OBD_PIDS)
        .copied()
        .collect::<BTreeSet<_>>();
    let extra_pids = pids
        .iter()
        .copied()
        .filter(|pid| !classified.contains(pid))
        .collect::<Vec<_>>();
    // Unknown advertised PIDs have no known payload length, so keep them as
    // single requests. Their response can then be retained as an observation.
    let extras = extra_pids.iter().map(|pid| vec![*pid]).collect::<Vec<_>>();

    let mut plan = Vec::new();
    let rounds = medium.len().max(slow.len()).max(extras.len()).max(1);
    for index in 0..rounds {
        plan.extend(fast.iter().cloned());
        if let Some(batch) = medium.get(index) {
            plan.push(batch.clone());
        }
        plan.extend(fast.iter().cloned());
        if let Some(batch) = slow.get(index) {
            plan.push(batch.clone());
        }
        if let Some(batch) = extras.get(index) {
            plan.push(batch.clone());
        }
    }

    if plan.is_empty() {
        plan.extend(pids.iter().map(|pid| vec![*pid]));
    }
    plan
}

fn build_single_pid_fallback_plan(pids: &[u8]) -> Vec<Vec<u8>> {
    build_obd_poll_plan(pids)
        .into_iter()
        .flat_map(|batch| batch.into_iter().map(|pid| vec![pid]).collect::<Vec<_>>())
        .collect()
}

fn pid_batches(priority: &[u8], available: &[u8]) -> Vec<Vec<u8>> {
    let selected = priority
        .iter()
        .copied()
        .filter(|pid| available.contains(pid))
        .collect::<Vec<_>>();
    selected.chunks(6).map(<[u8]>::to_vec).collect()
}

fn format_obd_request(pids: &[u8]) -> String {
    format!(
        "01{}",
        pids.iter()
            .map(|pid| format!("{pid:02X}"))
            .collect::<String>()
    )
}

fn mode_01_pid_data_len(pid: u8) -> Option<usize> {
    match pid {
        0x04..=0x07
        | 0x0B
        | 0x0D..=0x0F
        | 0x11
        | 0x2F
        | 0x33
        | 0x45..=0x47
        | 0x49
        | 0x4A
        | 0x4C
        | 0x5C => Some(1),
        0x0C | 0x10 | 0x1F | 0x21 | 0x31 | 0x3C | 0x42 | 0x43 => Some(2),
        _ => None,
    }
}

fn parse_elm327_obd_response_for_pids(response: &str, requested: &[u8]) -> Vec<CanFrame> {
    let mut payload = Vec::new();
    let mut found_service = false;

    for line in elm327_response_lines(response) {
        let content = line
            .split_once(':')
            .filter(|(prefix, _)| prefix.trim().len() <= 2)
            .map_or(line, |(_, content)| content);
        let bytes = elm327_hex_bytes(content);
        if !found_service {
            if let Some(position) = bytes.iter().position(|byte| *byte == 0x41) {
                payload.extend_from_slice(&bytes[position..]);
                found_service = true;
            }
        } else {
            payload.extend(bytes);
        }
    }

    let mut frames = Vec::new();
    let mut cursor = 1;
    for &pid in requested {
        if payload.get(cursor) == Some(&0x41) {
            cursor += 1;
        }
        let Some(relative_position) = payload
            .get(cursor..)
            .and_then(|remaining| remaining.iter().position(|candidate| *candidate == pid))
        else {
            continue;
        };
        cursor += relative_position + 1;
        let Some(data_len) = mode_01_pid_data_len(pid) else {
            continue;
        };
        let Some(data) = payload.get(cursor..cursor + data_len) else {
            break;
        };
        frames.push(CanFrame::new(u32::from(pid), false, false, data.to_vec()));
        cursor += data_len;
    }
    frames
}

fn initialize_elm327(port: &mut Box<dyn SerialPort>) -> Result<()> {
    let _ = port.set_timeout(Duration::from_millis(250));
    let _ = port.set_flow_control(FlowControl::None);
    let _ = port.write_data_terminal_ready(true);
    let _ = port.write_request_to_send(true);
    thread::sleep(Duration::from_millis(150));
    let _ = port.clear(ClearBuffer::All);
    wake_elm327(port);

    let reset_response =
        send_elm327_command_with_retries(port, "ATZ", Duration::from_millis(4_000), 3)?;
    if !is_elm327_ready_response(&reset_response) {
        anyhow::bail!("ELM327/STN adapter did not answer to ATZ: {reset_response:?}");
    }

    for command in [
        "ATE0", "ATL0", "ATS0", "ATH0", "ATAL", "ATCAF1", "ATAT1", "ATST96", "ATSP6",
    ] {
        let response =
            send_elm327_command_with_retries(port, command, Duration::from_millis(2_000), 2)?;
        if is_elm327_error_response(&response) {
            anyhow::bail!("ELM327 init command failed: {command}: {response:?}");
        }
    }

    Ok(())
}

fn send_elm327_command_with_retries(
    port: &mut Box<dyn SerialPort>,
    command: &str,
    timeout: Duration,
    attempts: usize,
) -> Result<String> {
    let mut last_response = String::new();

    for _ in 0..attempts {
        last_response = send_elm327_command(port, command, timeout)?;
        if is_elm327_ready_response(&last_response) {
            return Ok(last_response);
        }
        wake_elm327(port);
    }

    Ok(last_response)
}

fn send_elm327_command(
    port: &mut Box<dyn SerialPort>,
    command: &str,
    timeout: Duration,
) -> Result<String> {
    write_elm327_command(port, command)?;
    let response = read_elm327_response(port, timeout);
    tracing::debug!(command, response, "ELM327 command response");
    Ok(response)
}

fn is_elm327_ready_response(response: &str) -> bool {
    response.contains('>')
        && response
            .chars()
            .any(|character| character.is_ascii_alphabetic())
        && !response.contains('\u{FFFD}')
}

fn is_elm327_error_response(response: &str) -> bool {
    elm327_response_lines(response).any(|line| {
        let line = line.trim();
        line == "?"
            || line.eq_ignore_ascii_case("NO DATA")
            || line.eq_ignore_ascii_case("STOPPED")
            || line.eq_ignore_ascii_case("UNABLE TO CONNECT")
            || line.eq_ignore_ascii_case("BUS INIT: ERROR")
            || line.eq_ignore_ascii_case("CAN ERROR")
            || line.eq_ignore_ascii_case("BUFFER FULL")
            || line.eq_ignore_ascii_case("DATA ERROR")
    })
}

fn is_elm327_no_data_response(response: &str) -> bool {
    elm327_response_lines(response).any(|line| line.eq_ignore_ascii_case("NO DATA"))
}

fn wake_elm327(port: &mut Box<dyn SerialPort>) {
    let _ = port.write_all(b"\r");
    let _ = port.flush();
    let _ = read_elm327_response(port, Duration::from_millis(500));
    let _ = port.clear(ClearBuffer::Input);
}

fn elm327_response_lines(response: &str) -> impl Iterator<Item = &str> {
    response
        .split(['\r', '\n', '>'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
}

fn write_elm327_command(port: &mut Box<dyn SerialPort>, command: &str) -> Result<()> {
    port.write_all(command.as_bytes())
        .with_context(|| format!("ELM327コマンドを書き込めませんでした: {command}"))?;
    port.write_all(b"\r")
        .with_context(|| format!("ELM327コマンド終端を書き込めませんでした: {command}"))?;
    port.flush()
        .with_context(|| format!("ELM327コマンドを送信できませんでした: {command}"))?;
    Ok(())
}

fn read_elm327_response(port: &mut Box<dyn SerialPort>, timeout: Duration) -> String {
    let started_at = Instant::now();
    let mut buffer = [0_u8; 256];
    let mut response = Vec::new();

    while started_at.elapsed() < timeout {
        match port.read(&mut buffer) {
            Ok(0) => {}
            Ok(read_size) => {
                response.extend_from_slice(&buffer[..read_size]);
                if buffer[..read_size].contains(&b'>') {
                    break;
                }
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }

    String::from_utf8_lossy(&response).to_string()
}

fn parse_supported_pid_bits(base_pid: u8, data: &[u8]) -> BTreeSet<u8> {
    let mut supported = BTreeSet::new();
    let Some(bytes) = data.get(..4) else {
        return supported;
    };

    for (byte_index, byte) in bytes.iter().enumerate() {
        for bit_index in 0..8 {
            if byte & (0x80 >> bit_index) != 0 {
                supported.insert(base_pid + (byte_index as u8 * 8) + bit_index + 1);
                let pid = u16::from(base_pid) + (byte_index as u16 * 8) + bit_index as u16 + 1;
                if let Ok(pid) = u8::try_from(pid) {
                    supported.insert(pid);
                }
            }
        }
    }

    supported
}

fn format_pid_set(pids: &BTreeSet<u8>) -> String {
    format_pid_list(&pids.iter().copied().collect::<Vec<_>>())
}

fn format_pid_list(pids: &[u8]) -> String {
    pids.iter()
        .map(|pid| format!("0x{pid:02X}"))
        .collect::<Vec<_>>()
        .join(", ")
}

struct SerialFrameParser {
    mode: ConnectionMode,
    line_buffer: Vec<u8>,
    frames: VecDeque<CanFrame>,
}

impl SerialFrameParser {
    fn new(mode: ConnectionMode) -> Self {
        Self {
            mode,
            line_buffer: Vec::new(),
            frames: VecDeque::new(),
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            match byte {
                b'\r' | b'\n' | b'>' => self.flush_line(),
                byte => self.line_buffer.push(*byte),
            }
        }
    }

    fn next_frame(&mut self) -> Option<CanFrame> {
        self.frames.pop_front()
    }

    fn flush_line(&mut self) {
        if self.line_buffer.is_empty() {
            return;
        }

        let line = String::from_utf8_lossy(&self.line_buffer).to_string();
        self.line_buffer.clear();

        let frame = match self.mode {
            ConnectionMode::Obd2 => parse_elm327_obd_response(&line),
            ConnectionMode::Stream => parse_asc_stream_frame(&line),
        };

        if let Some(frame) = frame {
            self.frames.push_back(frame);
        }
    }
}

fn parse_elm327_obd_response(line: &str) -> Option<CanFrame> {
    let normalized = line.trim();
    if normalized.is_empty() || normalized.starts_with("SEARCHING") || normalized.starts_with("NO ")
    {
        return None;
    }

    let bytes = elm327_hex_bytes(normalized);
    let service_position = bytes.windows(2).position(|window| window[0] == 0x41)?;
    let pid = bytes.get(service_position + 1).copied()?;
    let data_start = service_position + 2;
    let data = bytes.get(data_start..)?.to_vec();

    if data.is_empty() {
        return None;
    }

    Some(CanFrame::new(u32::from(pid), false, false, data))
}

fn elm327_hex_bytes(line: &str) -> Vec<u8> {
    line.split(|character: char| !character.is_ascii_hexdigit())
        .filter(|token| !token.is_empty())
        .flat_map(|token| {
            if token.len() == 3 {
                Vec::new()
            } else if token.len() % 2 == 0 {
                token
                    .as_bytes()
                    .chunks(2)
                    .filter_map(|pair| {
                        std::str::from_utf8(pair)
                            .ok()
                            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                    })
                    .collect()
            } else {
                u8::from_str_radix(token, 16).ok().into_iter().collect()
            }
        })
        .collect()
}

pub fn parse_monitor_status(response: &str) -> std::result::Result<(bool, u8), String> {
    if is_elm327_no_data_response(response) {
        return Err("NO DATA".into());
    }
    for line in elm327_response_lines(response) {
        let bytes = elm327_hex_bytes(line);
        if let Some(position) = bytes.windows(2).position(|pair| pair == [0x41, 0x01]) {
            let Some(status) = bytes.get(position + 2) else {
                return Err("incomplete Mode 01 PID 01 response".into());
            };
            return Ok((status & 0x80 != 0, status & 0x7f));
        }
    }
    Err("Mode 01 PID 01 response could not be parsed".into())
}

pub fn parse_mode_03_dtcs(response: &str) -> std::result::Result<Vec<DtcReading>, String> {
    if is_elm327_no_data_response(response) {
        return Err("NO DATA".into());
    }
    let mut result = Vec::new();
    let mut found = false;
    for line in elm327_response_lines(response) {
        let ecu = line
            .split_whitespace()
            .next()
            .filter(|token| token.len() == 3 && token.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_owned);
        let bytes = elm327_hex_bytes(line);
        let Some(service) = bytes.iter().position(|byte| *byte == 0x43) else {
            continue;
        };
        found = true;
        let payload = &bytes[service + 1..];
        if !payload.len().is_multiple_of(2) {
            return Err("incomplete Mode 03 response".into());
        }
        for pair in payload.chunks_exact(2) {
            if pair == [0, 0] {
                continue;
            }
            let family = ['P', 'C', 'B', 'U'][usize::from(pair[0] >> 6)];
            result.push(DtcReading {
                code: format!(
                    "{family}{:X}{:X}{:02X}",
                    (pair[0] >> 4) & 0x03,
                    pair[0] & 0x0f,
                    pair[1]
                ),
                ecu: ecu.clone(),
            });
        }
    }
    if found {
        result.sort_by(|a, b| (&a.code, &a.ecu).cmp(&(&b.code, &b.ecu)));
        result.dedup();
        Ok(result)
    } else {
        Err("Mode 03 response could not be parsed".into())
    }
}

fn parse_asc_stream_frame(line: &str) -> Option<CanFrame> {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    let data_marker_index = parts
        .iter()
        .position(|part| part.eq_ignore_ascii_case("d"))?;

    if data_marker_index < 3 {
        return None;
    }

    let id_token = parts[data_marker_index - 2];
    let is_extended = id_token.ends_with(['x', 'X']);
    let id_hex = id_token.trim_end_matches(['x', 'X']);
    let id = u32::from_str_radix(id_hex, 16).ok()?;

    let dlc = parts
        .get(data_marker_index + 1)
        .and_then(|value| value.parse::<usize>().ok())?;
    let data = parts
        .iter()
        .skip(data_marker_index + 2)
        .take(dlc)
        .filter_map(|byte| u8::from_str_radix(byte, 16).ok())
        .collect::<Vec<_>>();

    if data.len() != dlc {
        return None;
    }

    Some(CanFrame::new(id, is_extended, false, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_elm327_response_without_header() {
        let frame = parse_elm327_obd_response("41 0C 1A F8").unwrap();

        assert_eq!(frame.id, 0x0C);
        assert_eq!(frame.data, vec![0x1A, 0xF8]);
    }

    #[test]
    fn parse_elm327_response_with_can_header_and_length() {
        let frame = parse_elm327_obd_response("7E8 04 41 0D 28 00").unwrap();

        assert_eq!(frame.id, 0x0D);
        assert_eq!(frame.data, vec![0x28, 0x00]);
    }

    #[test]
    fn parse_compact_elm327_response() {
        let frame = parse_elm327_obd_response("410C1AF8").unwrap();

        assert_eq!(frame.id, 0x0C);
        assert_eq!(frame.data, vec![0x1A, 0xF8]);
    }

    #[test]
    fn parse_mode_01_supported_pid_bits() {
        let supported = parse_supported_pid_bits(0x00, &[0x18, 0x18, 0x00, 0x01]);

        assert!(supported.contains(&0x04));
        assert!(supported.contains(&0x05));
        assert!(supported.contains(&0x0C));
        assert!(supported.contains(&0x0D));
        assert!(supported.contains(&0x20));
        assert!(!supported.contains(&0x11));
    }

    #[test]
    fn standard_poll_list_is_not_filtered_by_supported_bitmap() {
        let supported = BTreeSet::from([0x0C, 0x0D]);
        let poll_pids = obd_poll_pids(&supported);

        assert_eq!(&poll_pids[..4], DASHBOARD_OBD_PIDS);
        assert!(poll_pids.contains(&0x2F));
        assert!(poll_pids.contains(&0x5C));
        assert!(
            STANDARD_OBD_POLL_PIDS
                .iter()
                .all(|pid| poll_pids.contains(pid))
        );
    }

    #[test]
    fn advertised_nonstandard_pid_is_appended_once() {
        let supported = BTreeSet::from([0x0C, 0x19]);
        let poll_pids = obd_poll_pids(&supported);

        assert_eq!(poll_pids.iter().filter(|pid| **pid == 0x0C).count(), 1);
        assert_eq!(poll_pids.iter().filter(|pid| **pid == 0x19).count(), 1);
    }

    #[test]
    fn prioritized_plan_polls_fast_pids_more_often() {
        let plan = build_obd_poll_plan(&standard_obd_poll_pids());
        let fast_count = plan.iter().filter(|batch| batch.contains(&0x0C)).count();
        let coolant_count = plan.iter().filter(|batch| batch.contains(&0x05)).count();

        assert!(fast_count > coolant_count);
        assert!(plan.iter().all(|batch| batch.len() <= 6));
        assert_eq!(plan[0], FAST_OBD_PIDS);
    }

    #[test]
    fn formats_multi_pid_request() {
        assert_eq!(format_obd_request(&[0x0C, 0x0D, 0x05, 0x11]), "010C0D0511");
    }

    #[test]
    fn parses_multi_pid_response_into_individual_frames() {
        let frames = parse_elm327_obd_response_for_pids(
            "010C0D0511\r0: 41 0C 17 B8 0D 28\r1: 05 44 11 80 00 00\r>",
            &[0x0C, 0x0D, 0x05, 0x11],
        );

        assert_eq!(frames.len(), 4);
        assert_eq!((frames[0].id, &frames[0].data), (0x0C, &vec![0x17, 0xB8]));
        assert_eq!((frames[1].id, &frames[1].data), (0x0D, &vec![0x28]));
        assert_eq!((frames[2].id, &frames[2].data), (0x05, &vec![0x44]));
        assert_eq!((frames[3].id, &frames[3].data), (0x11, &vec![0x80]));
    }

    #[test]
    fn single_pid_fallback_keeps_priority_weighting() {
        let plan = build_single_pid_fallback_plan(&standard_obd_poll_pids());

        assert!(plan.iter().all(|batch| batch.len() == 1));
        assert!(
            plan.iter().filter(|batch| batch[0] == 0x0C).count()
                > plan.iter().filter(|batch| batch[0] == 0x05).count()
        );
    }

    #[test]
    fn detects_no_data_response_case_insensitively() {
        assert!(is_elm327_no_data_response("012F\rNO DATA\r>"));
        assert!(is_elm327_no_data_response("no data\r>"));
        assert!(!is_elm327_no_data_response("41 2F 80\r>"));
        assert!(!is_elm327_no_data_response("CAN ERROR\r>"));
    }

    #[test]
    fn parses_monitor_status_mil_and_count() {
        assert_eq!(
            parse_monitor_status("41 01 82 07 E0 00").unwrap(),
            (true, 2)
        );
        assert_eq!(
            parse_monitor_status("7E8 06 41 01 00 00 00 00").unwrap(),
            (false, 0)
        );
        assert!(parse_monitor_status("41 01").is_err());
        assert_eq!(parse_monitor_status("NO DATA"), Err("NO DATA".into()));
    }

    #[test]
    fn parses_mode_03_single_multiple_and_families() {
        let single = parse_mode_03_dtcs("43 01 33 00 00").unwrap();
        assert_eq!(single[0].code, "P0133");
        let all = parse_mode_03_dtcs("7E8 09 43 01 33 41 23 81 AB C1 01").unwrap();
        assert_eq!(
            all.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(),
            vec!["B01AB", "C0123", "P0133", "U0101"]
        );
    }

    #[test]
    fn parses_mode_03_empty_and_rejects_incomplete() {
        assert!(parse_mode_03_dtcs("43 00 00 00 00").unwrap().is_empty());
        assert!(parse_mode_03_dtcs("43 01").is_err());
        assert_eq!(parse_mode_03_dtcs("NO DATA"), Err("NO DATA".into()));
    }

    #[test]
    fn diagnostics_are_low_priority_and_failure_does_not_disable_normal_polling() {
        let mut scheduler = DiagnosticScheduler::new(DiagnosticPollingConfig {
            interval: Duration::ZERO,
            normal_requests_between_diagnostics: 2,
            request_timeout: Duration::from_millis(1),
        });
        assert!(!scheduler.ready());
        scheduler.note_normal_request();
        assert!(!scheduler.ready());
        scheduler.note_normal_request();
        assert!(scheduler.ready());
        scheduler.consume_turn();
        // A diagnostic timeout/parse failure does not touch the normal request
        // counter or poll plan; another two normal turns must happen first.
        assert!(!scheduler.ready());
        scheduler.note_normal_request();
        scheduler.note_normal_request();
        assert!(scheduler.ready());
        scheduler.unsupported = true;
        assert!(!scheduler.ready());
    }

    #[test]
    fn parses_single_and_multiline_mode_09_vin() {
        assert_eq!(
            parse_mode_09_vin("49 02 01 4A 46 31 5A 44 38 41 31 31 52 31 32 33 34 35 36 37>"),
            Some("JF1ZD8A11R1234567".into())
        );
        assert_eq!(
            parse_mode_09_vin(
                "0: 49 02 01 4A 46 31 5A 44\r1: 21 38 41 31 31 52 31 32\r2: 22 33 34 35 36 37>"
            ),
            Some("JF1ZD8A11R1234567".into())
        );
        assert_eq!(parse_mode_09_vin("NO DATA>"), None);
    }

    #[test]
    fn parse_asc_stream_standard_frame() {
        let frame =
            parse_asc_stream_frame("0.000000 1 123 Rx d 8 10 20 30 40 50 60 70 80").unwrap();

        assert_eq!(frame.id, 0x123);
        assert!(!frame.is_extended);
        assert_eq!(
            frame.data,
            vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]
        );
    }

    #[test]
    fn parse_asc_stream_extended_frame() {
        let frame = parse_asc_stream_frame("0.000000 1 18DAF110x Rx d 3 41 0C 10").unwrap();

        assert_eq!(frame.id, 0x18DAF110);
        assert!(frame.is_extended);
        assert_eq!(frame.data, vec![0x41, 0x0C, 0x10]);
    }
}
