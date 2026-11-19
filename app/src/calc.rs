//! Arithmetic in number entries: `1.5+0.25`, `3/2`, `10*0.0254`,
//! `(2-0.5)/2`. The four operators, parentheses and unary minus, with the
//! usual precedence. Anything else is an error, never a silent zero.

/// Evaluate `s`. `×`, `÷` and `−` are accepted for `*`, `/` and `-`.
pub fn eval(s: &str) -> Result<f64, String> {
    let toks = lex(s)?;
    if toks.is_empty() {
        return Err("Enter a number".into());
    }
    let mut p = Parser { toks: &toks, i: 0 };
    let v = p.sum()?;
    if p.i < toks.len() {
        return Err(match toks[p.i] {
            Tok::Close => "Unmatched )".into(),
            _ => "Missing operator".into(),
        });
    }
    if !v.is_finite() {
        return Err("Result is too large".into());
    }
    // -0 reads oddly on a DRO.
    Ok(if v == 0.0 { 0.0 } else { v })
}

/// Whether `s` is more than a plain number, so the keypad shows its result.
pub fn is_expression(s: &str) -> bool {
    let body = s.trim().strip_prefix(['-', '−']).unwrap_or(s.trim());
    body.contains(|c: char| "+-*/×÷−()".contains(c))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok {
    Num(f64),
    Op(char),
    Open,
    Close,
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut it = s.chars().peekable();
    while let Some(&c) = it.peek() {
        match c {
            ' ' => {
                it.next();
            }
            '0'..='9' | '.' => {
                let mut n = String::new();
                while let Some(&d) = it.peek().filter(|d| d.is_ascii_digit() || **d == '.') {
                    n.push(d);
                    it.next();
                }
                let v = n.parse::<f64>().map_err(|_| format!("Not a number: {n}"))?;
                out.push(Tok::Num(v));
            }
            '+' | '-' | '−' | '*' | '×' | '/' | '÷' => {
                it.next();
                out.push(Tok::Op(match c {
                    '−' => '-',
                    '×' => '*',
                    '÷' => '/',
                    c => c,
                }));
            }
            '(' => {
                it.next();
                out.push(Tok::Open);
            }
            ')' => {
                it.next();
                out.push(Tok::Close);
            }
            c => return Err(format!("Can't use \"{c}\" in a number")),
        }
    }
    Ok(out)
}

struct Parser<'a> {
    toks: &'a [Tok],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.i).copied()
    }

    fn sum(&mut self) -> Result<f64, String> {
        let mut v = self.product()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek() {
            self.i += 1;
            let r = self.product()?;
            v = if op == '+' { v + r } else { v - r };
        }
        Ok(v)
    }

    fn product(&mut self) -> Result<f64, String> {
        let mut v = self.unary()?;
        while let Some(Tok::Op(op @ ('*' | '/'))) = self.peek() {
            self.i += 1;
            let r = self.unary()?;
            if op == '/' && r == 0.0 {
                return Err("Can't divide by zero".into());
            }
            v = if op == '*' { v * r } else { v / r };
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(Tok::Op('-')) => {
                self.i += 1;
                Ok(-self.unary()?)
            }
            Some(Tok::Op('+')) => {
                self.i += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }

    fn atom(&mut self) -> Result<f64, String> {
        let t = self.peek();
        self.i += 1;
        match t {
            Some(Tok::Num(v)) => Ok(v),
            Some(Tok::Open) => {
                let v = self.sum()?;
                if self.peek() != Some(Tok::Close) {
                    return Err("Missing )".into());
                }
                self.i += 1;
                Ok(v)
            }
            Some(Tok::Close) => Err("Empty ( )".into()),
            Some(Tok::Op(_)) => Err("Two operators in a row".into()),
            None => Err("Ends with an operator".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> f64 {
        eval(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn numbers() {
        assert_eq!(ok("12.5"), 12.5);
        assert_eq!(ok("-3"), -3.0);
        assert_eq!(ok(".5"), 0.5);
        assert_eq!(ok("5."), 5.0);
        assert_eq!(ok("-0"), 0.0);
        assert!(ok("-0").is_sign_positive());
    }

    #[test]
    fn the_four_operators() {
        assert_eq!(ok("1.5+0.25"), 1.75);
        assert_eq!(ok("3/2"), 1.5);
        assert!((ok("10*0.0254") - 0.254).abs() < 1e-12);
        assert_eq!(ok("(2-0.5)/2"), 0.75);
        assert_eq!(ok("10 − 4 ÷ 2 × 3"), 4.0);
    }

    #[test]
    fn precedence_and_sign() {
        assert_eq!(ok("1+2*3"), 7.0);
        assert_eq!(ok("(1+2)*3"), 9.0);
        assert_eq!(ok("8/4/2"), 1.0);
        assert_eq!(ok("5-3-1"), 1.0);
        assert_eq!(ok("3*-2"), -6.0);
        assert_eq!(ok("-(2+3)"), -5.0);
        assert_eq!(ok("--2"), 2.0);
    }

    #[test]
    fn errors_are_rejected() {
        for s in ["", "  ", "1+", "*2", "1**2", "(1+2", "1+2)", "()", "1/0", "1/(2-2)", "1.2.3", "1e5", "abc", "2 3", "2(3)"] {
            assert!(eval(s).is_err(), "{s:?} should be an error");
        }
        assert_eq!(eval("1/0").unwrap_err(), "Can't divide by zero");
    }

    #[test]
    fn expression_detection() {
        assert!(!is_expression("12.5"));
        assert!(!is_expression("-12.5"));
        assert!(is_expression("1+2"));
        assert!(is_expression("-1-2"));
        assert!(is_expression("(3)"));
    }
}
