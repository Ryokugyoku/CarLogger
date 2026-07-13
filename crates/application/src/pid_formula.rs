use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FormulaError {
    #[error("empty formula")]
    Empty,
    #[error("invalid syntax at byte {0}")]
    Syntax(usize),
    #[error("response byte {0} is not available")]
    MissingByte(char),
    #[error("division by zero")]
    DivisionByZero,
    #[error("non-finite result")]
    NonFinite,
}

pub fn validate(formula: &str) -> Result<(), FormulaError> {
    // Eight bytes exercise every permitted byte variable without making
    // validation dependent on a particular ECU response.
    evaluate(formula, &[1; 8]).map(|_| ())
}

pub fn evaluate(formula: &str, data: &[u8]) -> Result<f64, FormulaError> {
    if formula.trim().is_empty() {
        return Err(FormulaError::Empty);
    }
    if formula.trim().eq_ignore_ascii_case("raw") {
        return Ok(data
            .iter()
            .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte)) as f64);
    }
    let mut parser = Parser {
        input: formula.as_bytes(),
        data,
        position: 0,
    };
    let value = parser.parse_bit_or()?;
    parser.skip();
    if parser.position != parser.input.len() {
        return Err(FormulaError::Syntax(parser.position));
    }
    if !value.is_finite() {
        return Err(FormulaError::NonFinite);
    }
    Ok(value)
}

struct Parser<'a> {
    input: &'a [u8],
    data: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn parse_bit_or(&mut self) -> Result<f64, FormulaError> {
        let mut value = self.parse_bit_xor()?;
        while self.take(b'|') {
            value = ((value as i64) | (self.parse_bit_xor()? as i64)) as f64;
        }
        Ok(value)
    }
    fn parse_bit_xor(&mut self) -> Result<f64, FormulaError> {
        let mut value = self.parse_bit_and()?;
        while self.take(b'^') {
            value = ((value as i64) ^ (self.parse_bit_and()? as i64)) as f64;
        }
        Ok(value)
    }
    fn parse_bit_and(&mut self) -> Result<f64, FormulaError> {
        let mut value = self.parse_shift()?;
        while self.take(b'&') {
            value = ((value as i64) & (self.parse_shift()? as i64)) as f64;
        }
        Ok(value)
    }
    fn parse_shift(&mut self) -> Result<f64, FormulaError> {
        let mut value = self.parse_expression()?;
        loop {
            if self.take_pair(b'<', b'<') {
                value = (value as i64).wrapping_shl(self.parse_expression()? as u32) as f64;
            } else if self.take_pair(b'>', b'>') {
                value = (value as i64).wrapping_shr(self.parse_expression()? as u32) as f64;
            } else {
                return Ok(value);
            }
        }
    }
    fn parse_expression(&mut self) -> Result<f64, FormulaError> {
        let mut value = self.parse_term()?;
        loop {
            if self.take(b'+') {
                value += self.parse_term()?;
            } else if self.take(b'-') {
                value -= self.parse_term()?;
            } else {
                return Ok(value);
            }
        }
    }
    fn parse_term(&mut self) -> Result<f64, FormulaError> {
        let mut value = self.parse_factor()?;
        loop {
            if self.take(b'*') {
                value *= self.parse_factor()?;
            } else if self.take(b'/') {
                let divisor = self.parse_factor()?;
                if divisor == 0.0 {
                    return Err(FormulaError::DivisionByZero);
                }
                value /= divisor;
            } else if self.take(b'%') {
                let divisor = self.parse_factor()?;
                if divisor == 0.0 {
                    return Err(FormulaError::DivisionByZero);
                }
                value %= divisor;
            } else {
                return Ok(value);
            }
        }
    }
    fn parse_factor(&mut self) -> Result<f64, FormulaError> {
        self.skip();
        if self.take(b'(') {
            let value = self.parse_bit_or()?;
            if !self.take(b')') {
                return Err(FormulaError::Syntax(self.position));
            }
            return Ok(value);
        }
        if self.take(b'-') {
            return Ok(-self.parse_factor()?);
        }
        if self.take(b'~') {
            return Ok((!(self.parse_factor()? as i64)) as f64);
        }
        match self.peek() {
            Some(b'A'..=b'H') | Some(b'a'..=b'h') => {
                let byte = self.input[self.position];
                self.position += 1;
                let index = (byte.to_ascii_uppercase() - b'A') as usize;
                self.data
                    .get(index)
                    .map(|value| f64::from(*value))
                    .ok_or(FormulaError::MissingByte((b'A' + index as u8) as char))
            }
            Some(b'0'..=b'9') | Some(b'.') => self.number(),
            _ => Err(FormulaError::Syntax(self.position)),
        }
    }
    fn number(&mut self) -> Result<f64, FormulaError> {
        let start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9') | Some(b'.')) {
            self.position += 1;
        }
        std::str::from_utf8(&self.input[start..self.position])
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(FormulaError::Syntax(start))
    }
    fn take(&mut self, byte: u8) -> bool {
        self.skip();
        if self.peek() == Some(byte) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    fn take_pair(&mut self, first: u8, second: u8) -> bool {
        self.skip();
        if self.input.get(self.position..self.position + 2) == Some([first, second].as_slice()) {
            self.position += 2;
            true
        } else {
            false
        }
    }
    fn skip(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.position += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_and_bit_operations_are_safe() {
        assert_eq!(evaluate("((A * 256) + B) / 4", &[0x1a, 0xf8]), Ok(1726.0));
        assert_eq!(evaluate("(A << 8) | B", &[0x12, 0x34]), Ok(0x1234 as f64));
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        assert_eq!(evaluate("A / 0", &[1]), Err(FormulaError::DivisionByZero));
        assert_eq!(evaluate("B + 1", &[1]), Err(FormulaError::MissingByte('B')));
        assert!(evaluate("system('rm')", &[1]).is_err());
        assert!(evaluate("1..2", &[1]).is_err());
    }
}
