//! `wait`/`expect` expressions: `SOURCE.path OP VALUE`.
//!
//! Sources: `stat.` (LinuxCNC's own status, `linuxcnc.stat()` fields),
//! `prop.` (millgui's mirrored Live properties), `hal.` (a HAL pin or signal),
//! `errors` (LinuxCNC error-channel texts so far), `ui-errors` (millgui's
//! error messages so far). Paths take `[i]` indexes, `.field`, and `.len`;
//! a path may end in `{TEXT}`: the number right after the last TEXT in a
//! text value (in a list, the last item containing TEXT), as in
//! `prop.pr-result{X width} ~= 40 +- 0.01`.
//! OPs: `==` `!=` `<` `<=` `>` `>=` `~=` (numbers; `~= 10 +- 0.01`, default
//! tolerance 0.002) `contains` `!contains` (strings, or any array element).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Stat,
    Prop,
    Hal(String),
    Errors,
    UiErrors,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub source: Source,
    pub path: Vec<Seg>,
    pub op: String,
    pub rhs: Value,
    pub tol: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Seg {
    Key(String),
    Index(usize),
    Len,
    /// The number after this text (`{TEXT}` at the end of a path).
    After(String),
}

const OPS: &[&str] = &[
    "!contains",
    "contains",
    "==",
    "!=",
    "<=",
    ">=",
    "~=",
    "<",
    ">",
];

pub fn parse(s: &str) -> Result<Expr, String> {
    let s = s.trim();
    let (lhs, op, rest) = OPS
        .iter()
        .filter_map(|op| find_op(s, op).map(|i| (i, *op)))
        .min_by_key(|(i, _)| *i)
        .map(|(i, op)| (s[..i].trim(), op, s[i + op.len()..].trim()))
        .ok_or_else(|| format!("no operator in '{s}'"))?;
    let (rhs, tol) = match rest.split_once("+-") {
        Some((v, t)) if op == "~=" => (
            v.trim(),
            t.trim()
                .parse::<f64>()
                .map_err(|_| format!("bad tolerance in '{s}'"))?,
        ),
        _ => (rest, 0.002),
    };
    let rhs = crate::script::value(rhs);
    // `{TEXT}` at the end: the number after TEXT.
    let (lhs, after) = match (lhs.find('{'), lhs.ends_with('}')) {
        (Some(i), true) => (&lhs[..i], Some(lhs[i + 1..lhs.len() - 1].to_string())),
        _ => (lhs, None),
    };
    let (source, mut path) = if let Some(p) = lhs.strip_prefix("stat.") {
        (Source::Stat, segs(p)?)
    } else if let Some(p) = lhs.strip_prefix("prop.") {
        (Source::Prop, segs(p)?)
    } else if let Some(p) = lhs.strip_prefix("hal.") {
        let (pin, path) = match p.find(['[']) {
            Some(i) => (&p[..i], segs(&p[i..])?),
            None => (p, vec![]),
        };
        (Source::Hal(pin.to_string()), path)
    } else if let Some(p) = lhs.strip_prefix("ui-errors") {
        (Source::UiErrors, segs(p.trim_start_matches('.'))?)
    } else if let Some(p) = lhs.strip_prefix("errors") {
        (Source::Errors, segs(p.trim_start_matches('.'))?)
    } else {
        return Err(format!(
            "'{lhs}': start with stat., prop., hal., errors or ui-errors"
        ));
    };
    path.extend(after.map(Seg::After));
    Ok(Expr {
        source,
        path,
        op: op.to_string(),
        rhs,
        tol,
    })
}

/// The first `op` with spaces around it (so `!=` is not read as `=`).
fn find_op(s: &str, op: &str) -> Option<usize> {
    let pat = format!(" {op} ");
    s.find(&pat).map(|i| i + 1)
}

fn segs(p: &str) -> Result<Vec<Seg>, String> {
    let mut out = Vec::new();
    for part in p.split('.').filter(|x| !x.is_empty()) {
        let (key, idx) = match part.find('[') {
            Some(i) => (&part[..i], &part[i..]),
            None => (part, ""),
        };
        if key == "len" {
            out.push(Seg::Len);
        } else if !key.is_empty() {
            out.push(Seg::Key(key.to_string()));
        }
        for n in idx.split('[').filter(|x| !x.is_empty()) {
            let n = n
                .trim_end_matches(']')
                .parse::<usize>()
                .map_err(|_| format!("bad index in '{p}'"))?;
            out.push(Seg::Index(n));
        }
    }
    Ok(out)
}

pub fn lookup(mut v: &Value, path: &[Seg]) -> Option<Value> {
    for (i, s) in path.iter().enumerate() {
        match s {
            Seg::Key(k) => v = v.get(k)?,
            Seg::Index(n) => v = v.get(*n)?,
            Seg::After(t) => {
                let s = match v {
                    Value::Array(a) => a.iter().rev().map(text).find(|s| s.contains(t.as_str()))?,
                    other => text(other),
                };
                return number_after(&s, t).filter(|_| i + 1 == path.len()).map(Value::from);
            }
            Seg::Len => {
                let n = match v {
                    Value::Array(a) => a.len(),
                    Value::String(s) => s.chars().count(),
                    Value::Object(o) => o.len(),
                    _ => return None,
                };
                return if i + 1 == path.len() {
                    Some(Value::from(n))
                } else {
                    None
                };
            }
        }
    }
    Some(v.clone())
}

/// The number that follows the last `t` in `s` (spaces, `=` and `:`
/// skipped): a routine's final value comes after any earlier one.
fn number_after(s: &str, t: &str) -> Option<f64> {
    let rest = &s[s.rfind(t)? + t.len()..];
    let rest = rest.trim_start_matches([' ', '=', ':']);
    let end = rest
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *i == 0)))
        .map_or(rest.len(), |(i, _)| i);
    rest[..end].parse().ok()
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        // DROs and HAL show numbers as text ("12.500", "TRUE").
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(1.0),
            "false" => Some(0.0),
            t => t.trim_end_matches(['m', 'i', 'n', '°', ' ']).parse().ok(),
        },
        _ => None,
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Whether `lhs OP rhs` holds.
pub fn holds(e: &Expr, lhs: &Value) -> bool {
    match e.op.as_str() {
        "contains" | "!contains" => {
            let needle = text(&e.rhs);
            let has = match lhs {
                Value::Array(a) => a.iter().any(|x| text(x).contains(&needle) || x == &e.rhs),
                other => text(other).contains(&needle),
            };
            has == (e.op == "contains")
        }
        "==" | "!=" => {
            let eq = match (num(lhs), num(&e.rhs)) {
                (Some(a), Some(b))
                    if !matches!((lhs, &e.rhs), (Value::String(_), Value::String(_))) =>
                {
                    (a - b).abs() < 1e-9
                }
                _ => lhs == &e.rhs || text(lhs) == text(&e.rhs),
            };
            eq == (e.op == "==")
        }
        op => match (num(lhs), num(&e.rhs)) {
            (Some(a), Some(b)) => match op {
                "<" => a < b,
                "<=" => a <= b,
                ">" => a > b,
                ">=" => a >= b,
                "~=" => (a - b).abs() <= e.tol,
                _ => false,
            },
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exprs() {
        let st =
            json!({"homed": [1, 1, 0], "position": [10.0004, -2.0, 0.0], "file": "/x/square.ngc"});
        let e = parse("stat.homed[1] == 1").unwrap();
        assert!(holds(&e, &lookup(&st, &e.path).unwrap()));
        let e = parse("stat.position[0] ~= 10").unwrap();
        assert!(holds(&e, &lookup(&st, &e.path).unwrap()));
        let e = parse("stat.position[1] ~= -2.1 +- 0.05").unwrap();
        assert!(!holds(&e, &lookup(&st, &e.path).unwrap()));
        let e = parse("stat.file contains square").unwrap();
        assert!(holds(&e, &lookup(&st, &e.path).unwrap()));
        let e = parse("stat.homed.len == 3").unwrap();
        assert!(holds(&e, &lookup(&st, &e.path).unwrap()));
        let e = parse("hal.halui.mode.is-auto == TRUE").unwrap();
        assert_eq!(e.source, Source::Hal("halui.mode.is-auto".into()));
        assert!(holds(&e, &json!("TRUE")));
        let e = parse("prop.dro-pos[0].work ~= 1.5").unwrap();
        assert!(holds(
            &e,
            &lookup(&json!({"dro-pos": [{"work": "1.500"}]}), &e.path).unwrap()
        ));
        let e = parse("prop.tool != \"T0\"").unwrap();
        assert!(holds(&e, &json!("T3")));
        let e = parse("prop.pr-result{X width} ~= 40 +- 0.01").unwrap();
        assert_eq!(e.path.last(), Some(&Seg::After("X width".into())));
        let r = json!({"pr-result": "X width 3.1\nX center 330.000\nX width 40.004\nWritten to G54"});
        assert!(holds(&e, &lookup(&r, &e.path).unwrap()));
        let e = parse("prop.messages{P3032=} ~= 0.059 +- 0.001").unwrap();
        let m = json!({"messages": ["P3032=0.5", "other", "P3032=0.059025"]});
        assert!(holds(&e, &lookup(&m, &e.path).unwrap()));
        let e = parse("prop.pr-result{Stock} ~= -1.5").unwrap();
        assert!(holds(&e, &lookup(&json!({"pr-result": "Stock -1.5 × 2"}), &e.path).unwrap()));
        let e = parse("errors !contains limit").unwrap();
        assert!(holds(&e, &json!(["Probe tripped"])));
    }
}
