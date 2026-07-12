use std::collections::HashMap;

use car_logger_domain::{CanFrame, DecodedSignalValue, SignalDefinition, SignalKind};
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

pub fn evaluate_formula(formula: &str, data: &[u8]) -> Option<f64> {
    let formula = formula.trim();
    if formula.eq_ignore_ascii_case("raw") {
        return Some(raw_value(data));
    }

    let mut parser = FormulaParser::new(formula, data);
    let value = parser.parse_expression()?;
    parser.skip_whitespace();

    if parser.is_finished() {
        Some(value)
    } else {
        None
    }
}

fn raw_value(data: &[u8]) -> f64 {
    data.iter()
        .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte)) as f64
}

struct FormulaParser<'a> {
    input: &'a [u8],
    data: &'a [u8],
    position: usize,
}

impl<'a> FormulaParser<'a> {
    fn new(input: &'a str, data: &'a [u8]) -> Self {
        Self {
            input: input.as_bytes(),
            data,
            position: 0,
        }
    }

    fn parse_expression(&mut self) -> Option<f64> {
        let mut value = self.parse_term()?;

        loop {
            self.skip_whitespace();
            match self.peek() {
                Some(b'+') => {
                    self.position += 1;
                    value += self.parse_term()?;
                }
                Some(b'-') => {
                    self.position += 1;
                    value -= self.parse_term()?;
                }
                _ => return Some(value),
            }
        }
    }

    fn parse_term(&mut self) -> Option<f64> {
        let mut value = self.parse_factor()?;

        loop {
            self.skip_whitespace();
            match self.peek() {
                Some(b'*') => {
                    self.position += 1;
                    value *= self.parse_factor()?;
                }
                Some(b'/') => {
                    self.position += 1;
                    value /= self.parse_factor()?;
                }
                _ => return Some(value),
            }
        }
    }

    fn parse_factor(&mut self) -> Option<f64> {
        self.skip_whitespace();

        match self.peek()? {
            b'(' => {
                self.position += 1;
                let value = self.parse_expression()?;
                self.skip_whitespace();
                if self.consume(b')') {
                    Some(value)
                } else {
                    None
                }
            }
            b'-' => {
                self.position += 1;
                self.parse_factor().map(|value| -value)
            }
            b'A'..=b'H' | b'a'..=b'h' => self.parse_variable(),
            b'0'..=b'9' | b'.' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_variable(&mut self) -> Option<f64> {
        let variable = self.input.get(self.position).copied()?;
        self.position += 1;
        let index = variable.to_ascii_uppercase().checked_sub(b'A')? as usize;

        self.data.get(index).map(|byte| f64::from(*byte))
    }

    fn parse_number(&mut self) -> Option<f64> {
        let start = self.position;
        while let Some(byte) = self.input.get(self.position) {
            if byte.is_ascii_digit() || *byte == b'.' {
                self.position += 1;
            } else {
                break;
            }
        }

        std::str::from_utf8(&self.input[start..self.position])
            .ok()?
            .parse::<f64>()
            .ok()
    }

    fn skip_whitespace(&mut self) {
        while self
            .input
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    fn is_finished(&self) -> bool {
        self.position == self.input.len()
    }
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
