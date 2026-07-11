BEGIN IMMEDIATE;

-- Refresh the built-in SAE OBD-II Mode 01 PID definitions in an existing
-- CarLogger SQLite master database. Existing definitions for these PID IDs
-- are intentionally replaced; definitions for other PID IDs are preserved.
INSERT INTO signal_definitions (
    signal_type,
    signal_id,
    name,
    unit,
    formula,
    updated_at
)
VALUES
    ('PID', 0x04, 'Calculated engine load',               '%',    'A*100/255',            CURRENT_TIMESTAMP),
    ('PID', 0x05, 'Engine coolant temperature',           'degC', 'A-40',                 CURRENT_TIMESTAMP),
    ('PID', 0x06, 'Short term fuel trim bank 1',          '%',    'A*100/128-100',        CURRENT_TIMESTAMP),
    ('PID', 0x07, 'Long term fuel trim bank 1',           '%',    'A*100/128-100',        CURRENT_TIMESTAMP),
    ('PID', 0x0B, 'Intake manifold absolute pressure',    'kPa',  'A',                    CURRENT_TIMESTAMP),
    ('PID', 0x0C, 'Engine RPM',                           'rpm',  '((A*256)+B)/4',        CURRENT_TIMESTAMP),
    ('PID', 0x0D, 'Vehicle speed',                        'km/h', 'A',                    CURRENT_TIMESTAMP),
    ('PID', 0x0E, 'Timing advance',                       'deg',  'A/2-64',               CURRENT_TIMESTAMP),
    ('PID', 0x0F, 'Intake air temperature',               'degC', 'A-40',                 CURRENT_TIMESTAMP),
    ('PID', 0x10, 'Mass air flow rate',                   'g/s',  '((A*256)+B)/100',      CURRENT_TIMESTAMP),
    ('PID', 0x11, 'Throttle position',                    '%',    'A*100/255',            CURRENT_TIMESTAMP),
    ('PID', 0x1F, 'Run time since engine start',          's',    '(A*256)+B',            CURRENT_TIMESTAMP),
    ('PID', 0x21, 'Distance with MIL on',                 'km',   '(A*256)+B',            CURRENT_TIMESTAMP),
    ('PID', 0x2F, 'Fuel tank level input',                '%',    'A*100/255',            CURRENT_TIMESTAMP),
    ('PID', 0x31, 'Distance since DTCs cleared',          'km',   '(A*256)+B',            CURRENT_TIMESTAMP),
    ('PID', 0x33, 'Barometric pressure',                  'kPa',  'A',                    CURRENT_TIMESTAMP),
    ('PID', 0x3C, 'Catalyst temperature bank 1 sensor 1', 'degC', '((A*256)+B)/10-40',    CURRENT_TIMESTAMP),
    ('PID', 0x42, 'Control module voltage',               'V',    '((A*256)+B)/1000',     CURRENT_TIMESTAMP),
    ('PID', 0x43, 'Absolute load value',                  '%',    '((A*256)+B)*100/255',  CURRENT_TIMESTAMP),
    ('PID', 0x45, 'Relative throttle position',           '%',    'A*100/255',            CURRENT_TIMESTAMP),
    ('PID', 0x46, 'Ambient air temperature',              'degC', 'A-40',                 CURRENT_TIMESTAMP),
    ('PID', 0x47, 'Absolute throttle position B',         '%',    'A*100/255',            CURRENT_TIMESTAMP),
    ('PID', 0x49, 'Accelerator pedal position D',         '%',    'A*100/255',            CURRENT_TIMESTAMP),
    ('PID', 0x4A, 'Accelerator pedal position E',         '%',    'A*100/255',            CURRENT_TIMESTAMP),
    ('PID', 0x4C, 'Commanded throttle actuator',          '%',    'A*100/255',            CURRENT_TIMESTAMP),
    ('PID', 0x5C, 'Engine oil temperature',               'degC', 'A-40',                 CURRENT_TIMESTAMP)
ON CONFLICT(signal_type, signal_id) DO UPDATE SET
    name = excluded.name,
    unit = excluded.unit,
    formula = excluded.formula,
    updated_at = excluded.updated_at;

COMMIT;
