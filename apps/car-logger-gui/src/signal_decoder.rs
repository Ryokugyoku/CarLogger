use std::collections::HashMap;

use car_logger_domain::{
    CanFrame, DecodedSignalValue, RealtimeSignalState, SignalDefinition, SignalKind,
};
use chrono::Utc;

pub type SignalDefinitionMap = HashMap<(SignalKind, u32), SignalDefinition>;

pub fn definition_map(definitions: Vec<SignalDefinition>) -> SignalDefinitionMap {
    definitions
        .into_iter()
        .map(|definition| ((definition.kind, definition.id), definition))
        .collect()
}

pub fn decode_frame(
    kind: SignalKind,
    frame: &CanFrame,
    definitions: &SignalDefinitionMap,
) -> Option<Vec<DecodedSignalValue>> {
    let definition = definitions.get(&(kind, frame.id))?;
    let value = evaluate_formula(&definition.formula, &frame.data)?;

    Some(vec![DecodedSignalValue {
        name: definition.name.clone(),
        value,
        unit: definition.unit.clone(),
        formula: definition.formula.clone(),
        updated_at: Utc::now(),
    }])
}

pub fn find_metric(snapshot: &[RealtimeSignalState], names: &[&str]) -> Option<DecodedSignalValue> {
    let values = || {
        snapshot
            .iter()
            .flat_map(|state| state.decoded_values.iter())
    };
    values()
        .find(|value| {
            names
                .iter()
                .any(|name| value.name.eq_ignore_ascii_case(name))
        })
        .or_else(|| {
            values().find(|value| {
                names
                    .iter()
                    .any(|name| value.name.to_ascii_lowercase().contains(name))
            })
        })
        .cloned()
}

pub fn evaluate_formula(formula: &str, data: &[u8]) -> Option<f64> {
    car_logger_application::pid_formula::evaluate(formula, data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_obd_rpm_formula() {
        assert_eq!(
            evaluate_formula("((A*256)+B)/4", &[0x1A, 0xF8]).unwrap(),
            1726.0
        );
    }

    #[test]
    fn evaluate_temperature_formula() {
        assert_eq!(evaluate_formula("A-40", &[90]).unwrap(), 50.0);
    }

    #[test]
    fn raw_formula_uses_big_endian_payload() {
        assert_eq!(
            evaluate_formula("raw", &[0x12, 0x34]).unwrap(),
            0x1234 as f64
        );
    }
}
