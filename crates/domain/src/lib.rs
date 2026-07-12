use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

pub type CanId = u32;

/// 通信方式に依存しない共通CANフレーム。
///
/// SocketCANやSerialライブラリ固有の型をDomainへ持ち込まない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanFrame {
    pub id: CanId,
    pub is_extended: bool,
    pub is_remote: bool,
    pub data: Vec<u8>,
    pub received_at: DateTime<Utc>,
}

impl CanFrame {
    pub fn new(id: u32, is_extended: bool, is_remote: bool, data: Vec<u8>) -> Self {
        Self {
            id,
            is_extended,
            is_remote,
            data,
            received_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalKind {
    CanId,
    Pid,
}

impl SignalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CanId => "CAN_ID",
            Self::Pid => "PID",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalDefinition {
    pub kind: SignalKind,
    pub id: u32,
    pub name: String,
    pub unit: Option<String>,
    pub formula: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanIdObservation {
    pub id: CanId,
    pub raw_payload: Vec<u8>,
    pub last_seen: DateTime<Utc>,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedSignalValue {
    pub name: String,
    pub value: f64,
    pub unit: Option<String>,
    pub formula: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealtimeSignalState {
    pub latest_frame: CanFrame,
    pub raw_payload: Vec<u8>,
    pub last_seen: DateTime<Utc>,
    pub count: u64,
    pub decoded_values: Vec<DecodedSignalValue>,
    pub is_known: bool,
}

impl RealtimeSignalState {
    pub fn unknown(frame: CanFrame, previous_count: u64) -> Self {
        Self {
            raw_payload: frame.data.clone(),
            last_seen: frame.received_at,
            count: previous_count + 1,
            latest_frame: frame,
            decoded_values: Vec::new(),
            is_known: false,
        }
    }

    pub fn known(
        frame: CanFrame,
        decoded_values: Vec<DecodedSignalValue>,
        previous_count: u64,
    ) -> Self {
        Self {
            raw_payload: frame.data.clone(),
            last_seen: frame.received_at,
            count: previous_count + 1,
            latest_frame: frame,
            decoded_values,
            is_known: true,
        }
    }
}

/// 最新状態表示専用の状態ストア。
///
/// 全履歴は保持せず、CAN IDごとに最後に見えたフレームとdecode済み値だけを差し替える。
#[derive(Debug, Default)]
pub struct RealtimeState {
    pub signals: DashMap<CanId, RealtimeSignalState>,
}

impl RealtimeState {
    pub fn new() -> Self {
        Self {
            signals: DashMap::new(),
        }
    }

    pub fn upsert_unknown(&self, frame: CanFrame) {
        let previous_count = self
            .signals
            .get(&frame.id)
            .map(|state| state.count)
            .unwrap_or_default();
        self.signals.insert(
            frame.id,
            RealtimeSignalState::unknown(frame, previous_count),
        );
    }

    pub fn upsert_known(&self, frame: CanFrame, decoded_values: Vec<DecodedSignalValue>) {
        let previous_count = self
            .signals
            .get(&frame.id)
            .map(|state| state.count)
            .unwrap_or_default();
        self.signals.insert(
            frame.id,
            RealtimeSignalState::known(frame, decoded_values, previous_count),
        );
    }

    pub fn snapshot(&self) -> Vec<(CanId, RealtimeSignalState)> {
        let mut rows = self
            .signals
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect::<Vec<_>>();
        rows.sort_by_key(|(id, _)| *id);
        rows
    }

    pub fn clear(&self) {
        self.signals.clear();
    }
}
