use std::time::Duration;

use car_logger_domain::{Vehicle, VehicleId};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);
pub const MAX_RECONNECT_ATTEMPTS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub connect_timeout: Duration,
    pub retry_interval: Duration,
    pub max_attempts: u8,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            retry_interval: RECONNECT_INTERVAL,
            max_attempts: MAX_RECONNECT_ATTEMPTS,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReconnectController {
    policy: ReconnectPolicy,
    attempt: u8,
    next_attempt_at: Option<Duration>,
    cancelled: bool,
}

impl ReconnectController {
    pub fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            attempt: 0,
            next_attempt_at: None,
            cancelled: false,
        }
    }
    pub fn on_disconnected(&mut self, now: Duration) {
        self.attempt = 0;
        self.cancelled = false;
        self.next_attempt_at = Some(now + self.policy.retry_interval);
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.next_attempt_at = None;
    }
    pub fn poll(&mut self, now: Duration) -> Option<u8> {
        if self.cancelled
            || self.attempt >= self.policy.max_attempts
            || self.next_attempt_at.is_none_or(|at| now < at)
        {
            return None;
        }
        self.attempt += 1;
        self.next_attempt_at =
            (self.attempt < self.policy.max_attempts).then_some(now + self.policy.retry_interval);
        Some(self.attempt)
    }
    pub fn exhausted(&self) -> bool {
        !self.cancelled && self.attempt >= self.policy.max_attempts
    }
    pub fn timeout(&self) -> Duration {
        self.policy.connect_timeout
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionTarget {
    pub interface: String,
    pub adapter: String,
    pub safe_settings_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting {
        target: ConnectionTarget,
        automatic: bool,
    },
    LinkEstablished {
        target: ConnectionTarget,
    },
    Identifying,
    Identified {
        vehicle_id: VehicleId,
    },
    RegistrationRequired {
        normalized_vin: Option<String>,
    },
    Ready {
        vehicle_id: VehicleId,
    },
    Reconnecting {
        target: ConnectionTarget,
        attempt: u8,
    },
    LinkLost,
    Cancelled,
    Error(String),
}

impl ConnectionState {
    pub fn connected_vehicle_id(&self) -> Option<VehicleId> {
        match self {
            Self::Identified { vehicle_id } | Self::Ready { vehicle_id } => Some(*vehicle_id),
            _ => None,
        }
    }

    pub fn permits_persistence(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    pub fn permits_health_or_realtime(&self) -> bool {
        self.permits_persistence()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentificationOutcome {
    ExistingVehicle { vehicle_id: VehicleId },
    SelectionRequired,
    RegistrationRequired { normalized_vin: Option<String> },
    InvalidVin(String),
}

pub fn normalize_vin(value: &str) -> Result<Option<String>, String> {
    let value = value.trim().to_ascii_uppercase();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() != 17
        || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        || value.contains(['I', 'O', 'Q'])
    {
        return Err("VIN must be 17 ASCII alphanumeric characters excluding I, O and Q".into());
    }
    Ok(Some(value))
}

pub fn identify_vehicle(vin: Option<&str>, vehicles: &[Vehicle]) -> IdentificationOutcome {
    let Some(vin) = vin else {
        return IdentificationOutcome::SelectionRequired;
    };
    let normalized = match normalize_vin(vin) {
        Ok(Some(value)) => value,
        Ok(None) => return IdentificationOutcome::SelectionRequired,
        Err(error) => return IdentificationOutcome::InvalidVin(error),
    };
    let mut matches = vehicles
        .iter()
        .filter(|vehicle| vehicle.deleted_at.is_none())
        .filter(|vehicle| vehicle.normalized_vin.as_deref() == Some(normalized.as_str()));
    match (matches.next(), matches.next()) {
        (Some(vehicle), None) => IdentificationOutcome::ExistingVehicle {
            vehicle_id: vehicle.id,
        },
        (None, _) => IdentificationOutcome::RegistrationRequired {
            normalized_vin: Some(normalized),
        },
        _ => IdentificationOutcome::SelectionRequired,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleSelectionState {
    pub connected_vehicle_id: Option<VehicleId>,
    pub viewed_vehicle_id: Option<VehicleId>,
}

impl VehicleSelectionState {
    pub fn set_connected(&mut self, vehicle_id: Option<VehicleId>) {
        self.connected_vehicle_id = vehicle_id;
    }

    pub fn set_viewed(&mut self, vehicle_id: Option<VehicleId>) {
        self.viewed_vehicle_id = vehicle_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use car_logger_domain::FuelType;

    fn vehicle(id: i64, vin: Option<&str>) -> Vehicle {
        Vehicle {
            id,
            display_name: format!("Vehicle {id}"),
            normalized_vin: vin.map(str::to_owned),
            fuel_type: FuelType::Gasoline,
            displacement_l: 2.0,
            tank_capacity_l: 50.0,
            manufacturer: None,
            model: None,
            model_year: None,
            engine: None,
            odometer_km: None,
            notes: None,
            deleted_at: None,
            purge_after: None,
        }
    }

    #[test]
    fn vin_is_normalized_and_existing_vehicle_is_selected() {
        let vehicles = vec![vehicle(7, Some("JF1ZD8A11R1234567"))];
        assert_eq!(
            identify_vehicle(Some(" jf1zd8a11r1234567 "), &vehicles),
            IdentificationOutcome::ExistingVehicle { vehicle_id: 7 }
        );
    }

    #[test]
    fn missing_vin_never_guesses_a_vehicle() {
        assert_eq!(
            identify_vehicle(None, &[vehicle(1, None)]),
            IdentificationOutcome::SelectionRequired
        );
    }

    #[test]
    fn connected_and_viewed_vehicle_are_independent() {
        let mut state = VehicleSelectionState::default();
        state.set_viewed(Some(1));
        state.set_connected(Some(2));
        assert_eq!(state.viewed_vehicle_id, Some(1));
        assert_eq!(state.connected_vehicle_id, Some(2));
    }

    #[test]
    fn reconnect_waits_five_seconds_and_stops_after_three_attempts() {
        let mut reconnect = ReconnectController::new(ReconnectPolicy::default());
        reconnect.on_disconnected(Duration::ZERO);
        assert_eq!(reconnect.poll(Duration::from_secs(4)), None);
        assert_eq!(reconnect.poll(Duration::from_secs(5)), Some(1));
        assert_eq!(reconnect.poll(Duration::from_secs(10)), Some(2));
        assert_eq!(reconnect.poll(Duration::from_secs(15)), Some(3));
        assert!(reconnect.exhausted());
        assert_eq!(reconnect.poll(Duration::from_secs(20)), None);
        assert_eq!(reconnect.timeout(), Duration::from_secs(10));
    }

    #[test]
    fn automatic_connection_can_be_cancelled_without_waiting() {
        let mut reconnect = ReconnectController::new(ReconnectPolicy::default());
        reconnect.on_disconnected(Duration::ZERO);
        reconnect.cancel();
        assert_eq!(reconnect.poll(Duration::from_secs(60)), None);
        assert!(!reconnect.exhausted());
    }
}
