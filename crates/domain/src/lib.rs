use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub type CanId = u32;
pub type VehicleId = i64;
pub type ConnectionSessionId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FuelType {
    Gasoline,
    Diesel,
    Hybrid,
    PlugInHybrid,
    Electric,
    Lpg,
    Other,
}

impl FuelType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gasoline => "gasoline",
            Self::Diesel => "diesel",
            Self::Hybrid => "hybrid",
            Self::PlugInHybrid => "plug_in_hybrid",
            Self::Electric => "electric",
            Self::Lpg => "lpg",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Vehicle {
    pub id: VehicleId,
    pub display_name: String,
    pub normalized_vin: Option<String>,
    pub fuel_type: FuelType,
    pub displacement_l: f64,
    pub tank_capacity_l: f64,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub model_year: Option<u16>,
    pub engine: Option<String>,
    pub odometer_km: Option<f64>,
    pub notes: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub purge_after: Option<DateTime<Utc>>,
}

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
