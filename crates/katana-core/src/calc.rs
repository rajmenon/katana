use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum CalcError {
    Empty,
    Unexpected(char),
    Trailing,
    DivZero,
    Domain,
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CalcError::Empty => write!(f, "empty expression"),
            CalcError::Unexpected(c) => write!(f, "unexpected '{c}'"),
            CalcError::Trailing => write!(f, "trailing input"),
            CalcError::DivZero => write!(f, "division by zero"),
            CalcError::Domain => write!(f, "domain error"),
        }
    }
}

impl std::error::Error for CalcError {}

/// Evaluate a calculator expression (`2^9`, `123*456`, `0x123`, `SQR(100)`, `HEX(255)`).
/// `HEX(n)` returns n as if it were interpreted as the integer value (same as n);
/// prefix `0x` parses hex. `HEX` as a function converts to the hex numeric value
/// by reading the decimal and returning it — callers display via format.
pub fn eval_calc(expr: &str) -> Result<f64, CalcError> {
    let mut p = Parser {
        src: expr.trim(),
        i: 0,
    };
    if p.src.is_empty() {
        return Err(CalcError::Empty);
    }
    let v = p.parse_expr()?;
    p.skip();
    if p.i != p.src.len() {
        return Err(CalcError::Trailing);
    }
    Ok(v)
}

struct Parser<'a> {
    src: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn skip(&mut self) {
        while self.i < self.src.len()
            && self.src.as_bytes()[self.i].is_ascii_whitespace()
        {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip();
        self.src[self.i..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        self.skip();
        let mut chs = self.src[self.i..].chars();
        let c = chs.next()?;
        self.i += c.len_utf8();
        Some(c)
    }

    fn parse_expr(&mut self) -> Result<f64, CalcError> {
        let mut v = self.parse_term()?;
        loop {
            match self.peek() {
                Some('+') => {
                    self.bump();
                    v += self.parse_term()?;
                }
                Some('-') => {
                    self.bump();
                    v -= self.parse_term()?;
                }
                _ => return Ok(v),
            }
        }
    }

    fn parse_term(&mut self) -> Result<f64, CalcError> {
        let mut v = self.parse_power()?;
        loop {
            match self.peek() {
                Some('*') => {
                    self.bump();
                    v *= self.parse_power()?;
                }
                Some('/') => {
                    self.bump();
                    let d = self.parse_power()?;
                    if d == 0.0 {
                        return Err(CalcError::DivZero);
                    }
                    v /= d;
                }
                _ => return Ok(v),
            }
        }
    }

    fn parse_power(&mut self) -> Result<f64, CalcError> {
        let base = self.parse_unary()?;
        if self.peek() == Some('^') {
            self.bump();
            let exp = self.parse_power()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn parse_unary(&mut self) -> Result<f64, CalcError> {
        match self.peek() {
            Some('+') => {
                self.bump();
                self.parse_unary()
            }
            Some('-') => {
                self.bump();
                Ok(-self.parse_unary()?)
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<f64, CalcError> {
        self.skip();
        if self.peek() == Some('(') {
            self.bump();
            let v = self.parse_expr()?;
            if self.bump() != Some(')') {
                return Err(CalcError::Unexpected(')'));
            }
            return Ok(v);
        }

        // function name
        if self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            let start = self.i;
            while self.i < self.src.len()
                && self.src.as_bytes()[self.i].is_ascii_alphabetic()
            {
                self.i += 1;
            }
            let name = self.src[start..self.i].to_ascii_uppercase();
            self.skip();
            if self.peek() == Some('(') {
                self.bump();
                let arg = self.parse_expr()?;
                if self.bump() != Some(')') {
                    return Err(CalcError::Unexpected(')'));
                }
                return apply_fn(&name, arg);
            }
            return Err(CalcError::Unexpected('A'));
        }

        self.parse_number()
    }

    fn parse_number(&mut self) -> Result<f64, CalcError> {
        self.skip();
        let rest = &self.src[self.i..];
        if rest.len() >= 2 && rest.as_bytes()[0] == b'0' && rest.as_bytes()[1] | 32 == b'x' {
            self.i += 2;
            let start = self.i;
            while self.i < self.src.len() && self.src.as_bytes()[self.i].is_ascii_hexdigit()
            {
                self.i += 1;
            }
            if start == self.i {
                return Err(CalcError::Unexpected('x'));
            }
            let n = u64::from_str_radix(&self.src[start..self.i], 16)
                .map_err(|_| CalcError::Domain)?;
            return Ok(n as f64);
        }

        let start = self.i;
        let bytes = self.src.as_bytes();
        while self.i < self.src.len() && bytes[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.i < self.src.len() && bytes[self.i] == b'.' {
            self.i += 1;
            while self.i < self.src.len() && bytes[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        if start == self.i {
            let c = self.peek().unwrap_or('?');
            return Err(CalcError::Unexpected(c));
        }
        self.src[start..self.i]
            .parse()
            .map_err(|_| CalcError::Domain)
    }
}

fn apply_fn(name: &str, arg: f64) -> Result<f64, CalcError> {
    match name {
        "SQR" | "SQRT" => {
            if arg < 0.0 {
                return Err(CalcError::Domain);
            }
            Ok(arg.sqrt())
        }
        "ABS" => Ok(arg.abs()),
        "HEX" => Ok(arg.trunc()),
        _ => Err(CalcError::Unexpected('F')),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evals_representative_exprs() {
        assert_eq!(eval_calc("2^9").unwrap(), 512.0);
        assert_eq!(eval_calc("123*456").unwrap(), 56088.0);
        assert_eq!(eval_calc("0x123").unwrap(), 0x123 as f64);
        assert_eq!(eval_calc("SQR(100)").unwrap(), 10.0);
        assert_eq!(eval_calc("(1+2)*3").unwrap(), 9.0);
    }

    #[test]
    fn rejects_div_zero() {
        assert_eq!(eval_calc("1/0"), Err(CalcError::DivZero));
    }
}
