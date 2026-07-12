use anyhow::{Context, Result};
use car_logger_domain::SignalKind;
use rusqlite::{Connection, params};

struct BuiltinSignalDefinition {
    id: u32,
    name: &'static str,
    unit: Option<&'static str>,
    formula: &'static str,
}

const BRZ_86_BUILTIN_PID_DEFINITIONS: &[BuiltinSignalDefinition] = &[
    BuiltinSignalDefinition {
        id: 0x04,
        name: "Calculated engine load",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x05,
        name: "Engine coolant temperature",
        unit: Some("degC"),
        formula: "A-40",
    },
    BuiltinSignalDefinition {
        id: 0x06,
        name: "Short term fuel trim bank 1",
        unit: Some("%"),
        formula: "A*100/128-100",
    },
    BuiltinSignalDefinition {
        id: 0x07,
        name: "Long term fuel trim bank 1",
        unit: Some("%"),
        formula: "A*100/128-100",
    },
    BuiltinSignalDefinition {
        id: 0x0B,
        name: "Intake manifold absolute pressure",
        unit: Some("kPa"),
        formula: "A",
    },
    BuiltinSignalDefinition {
        id: 0x0C,
        name: "Engine RPM",
        unit: Some("rpm"),
        formula: "((A*256)+B)/4",
    },
    BuiltinSignalDefinition {
        id: 0x0D,
        name: "Vehicle speed",
        unit: Some("km/h"),
        formula: "A",
    },
    BuiltinSignalDefinition {
        id: 0x0E,
        name: "Timing advance",
        unit: Some("deg"),
        formula: "A/2-64",
    },
    BuiltinSignalDefinition {
        id: 0x0F,
        name: "Intake air temperature",
        unit: Some("degC"),
        formula: "A-40",
    },
    BuiltinSignalDefinition {
        id: 0x10,
        name: "Mass air flow rate",
        unit: Some("g/s"),
        formula: "((A*256)+B)/100",
    },
    BuiltinSignalDefinition {
        id: 0x11,
        name: "Throttle position",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x1F,
        name: "Run time since engine start",
        unit: Some("s"),
        formula: "(A*256)+B",
    },
    BuiltinSignalDefinition {
        id: 0x21,
        name: "Distance with MIL on",
        unit: Some("km"),
        formula: "(A*256)+B",
    },
    BuiltinSignalDefinition {
        id: 0x2F,
        name: "Fuel tank level input",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x31,
        name: "Distance since DTCs cleared",
        unit: Some("km"),
        formula: "(A*256)+B",
    },
    BuiltinSignalDefinition {
        id: 0x33,
        name: "Barometric pressure",
        unit: Some("kPa"),
        formula: "A",
    },
    BuiltinSignalDefinition {
        id: 0x3C,
        name: "Catalyst temperature bank 1 sensor 1",
        unit: Some("degC"),
        formula: "((A*256)+B)/10-40",
    },
    BuiltinSignalDefinition {
        id: 0x42,
        name: "Control module voltage",
        unit: Some("V"),
        formula: "((A*256)+B)/1000",
    },
    BuiltinSignalDefinition {
        id: 0x43,
        name: "Absolute load value",
        unit: Some("%"),
        formula: "((A*256)+B)*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x45,
        name: "Relative throttle position",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x46,
        name: "Ambient air temperature",
        unit: Some("degC"),
        formula: "A-40",
    },
    BuiltinSignalDefinition {
        id: 0x47,
        name: "Absolute throttle position B",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x49,
        name: "Accelerator pedal position D",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x4A,
        name: "Accelerator pedal position E",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x4C,
        name: "Commanded throttle actuator",
        unit: Some("%"),
        formula: "A*100/255",
    },
    BuiltinSignalDefinition {
        id: 0x5C,
        name: "Engine oil temperature",
        unit: Some("degC"),
        formula: "A-40",
    },
    BuiltinSignalDefinition {
        id: 0xA6,
        name: "Odometer",
        unit: Some("km"),
        formula: "((A*16777216)+(B*65536)+(C*256)+D)/10",
    },
];

pub(crate) fn insert_builtin_pid_definitions(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare(
        r#"
        INSERT OR IGNORE INTO signal_definitions (
            signal_type,
            signal_id,
            name,
            unit,
            formula
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
    )?;

    for definition in BRZ_86_BUILTIN_PID_DEFINITIONS {
        statement
            .execute(params![
                SignalKind::Pid.as_str(),
                definition.id,
                definition.name,
                definition.unit,
                definition.formula,
            ])
            .with_context(|| {
                format!(
                    "ビルトインPID定義を挿入できませんでした: 0x{:02X}",
                    definition.id
                )
            })?;
    }

    Ok(())
}
