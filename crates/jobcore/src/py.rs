//! The bits of Python behaviour the ported code depends on: number
//! formatting, `str()`/`repr()`, `json.dumps`/`json.loads` (including their
//! error messages), `str.strip`, `str.title` and the exception text that
//! `job_helper.main()` reports.

use serde_json::{Map, Number, Value};
use std::fmt;

// -- errors -------------------------------------------------------------------

/// An exception raised by the ported code.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// `mymill_job.JobError`: one or more user-facing messages.
    Job(Vec<String>),
    /// Any other Python exception: its type name and `str(exc)`.
    Py(String, String),
}

impl Error {
    pub fn job<S: Into<String>>(msg: S) -> Error {
        Error::Job(vec![msg.into()])
    }
    pub fn py<K: Into<String>, M: Into<String>>(kind: K, msg: M) -> Error {
        Error::Py(kind.into(), msg.into())
    }
    pub fn key(key: &Value) -> Error {
        Error::py("KeyError", repr(key))
    }
    pub fn key_str(key: &str) -> Error {
        Error::py("KeyError", repr_str(key))
    }
    pub fn type_error<M: Into<String>>(msg: M) -> Error {
        Error::py("TypeError", msg)
    }
    pub fn value_error<M: Into<String>>(msg: M) -> Error {
        Error::py("ValueError", msg)
    }
    pub fn attribute(obj: &str, name: &str) -> Error {
        Error::py("AttributeError", format!("'{}' object has no attribute '{}'", obj, name))
    }
    /// An `OSError` subclass as Python words it for a path.
    pub fn io(err: &std::io::Error, path: &str) -> Error {
        use std::io::ErrorKind::*;
        let (kind, errno, text) = match err.kind() {
            NotFound => ("FileNotFoundError", 2, "No such file or directory"),
            PermissionDenied => ("PermissionError", 13, "Permission denied"),
            AlreadyExists => ("FileExistsError", 17, "File exists"),
            IsADirectory => ("IsADirectoryError", 21, "Is a directory"),
            NotADirectory => ("NotADirectoryError", 20, "Not a directory"),
            _ => {
                let code = err.raw_os_error().unwrap_or(5);
                return Error::py("OSError", format!("[Errno {}] {}: {}", code, err, repr_str(path)));
            }
        };
        Error::py(kind, format!("[Errno {}] {}: {}", errno, text, repr_str(path)))
    }
    pub fn is_os_error(&self) -> bool {
        matches!(self, Error::Py(k, _) if k.ends_with("Error") && (k == "OSError" || k == "FileNotFoundError"
            || k == "PermissionError" || k == "FileExistsError" || k == "IsADirectoryError"
            || k == "NotADirectoryError"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Job(errors) => write!(f, "{}", errors.join("\n")),
            Error::Py(kind, msg) => write!(f, "{}: {}", kind, msg),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

// -- numbers ------------------------------------------------------------------

/// `repr(float)`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let s = format!("{:e}", x);
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.as_str()),
    };
    let (mant, exp) = s.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-4..16).contains(&exp) {
        let decpt = exp + 1;
        if decpt <= 0 {
            out.push_str("0.");
            out.push_str(&"0".repeat((-decpt) as usize));
            out.push_str(&digits);
        } else if decpt as usize >= digits.len() {
            out.push_str(&digits);
            out.push_str(&"0".repeat(decpt as usize - digits.len()));
            out.push_str(".0");
        } else {
            out.push_str(&digits[..decpt as usize]);
            out.push('.');
            out.push_str(&digits[decpt as usize..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    }
    out
}

/// `'{:.Nf}'.format(x)`.
pub fn fixed(x: f64, places: usize) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{:.*}", places, x)
}

/// `'{:g}'.format(x)`.
pub fn general(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{:.5e}", x);
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if (-4..6).contains(&exp) {
        let text = format!("{:.*}", (5 - exp) as usize, x);
        strip_point_zeros(&text)
    } else {
        format!("{}e{}{:02}", strip_point_zeros(mant), if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn strip_point_zeros(text: &str) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text.to_string()
    }
}

/// `round(x, 1)`.
pub fn round1(x: f64) -> f64 {
    format!("{:.1}", x).parse().unwrap_or(x)
}

/// `int(math.ceil(x))`.
pub fn ceil_int(x: f64) -> Result<i64> {
    if x.is_nan() {
        return Err(Error::value_error("cannot convert float NaN to integer"));
    }
    if x.is_infinite() {
        return Err(Error::py("OverflowError", "cannot convert float infinity to integer"));
    }
    Ok(x.ceil() as i64)
}

/// `float(text)` for a string.
pub fn parse_float(text: &str) -> Result<f64> {
    let t = strip(text).replace('_', "");
    let lower = t.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    let ok_word = matches!(body, "inf" | "infinity" | "nan");
    let ok_num = !body.is_empty()
        && body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-'))
        && body.chars().next().map(|c| c.is_ascii_digit() || c == '.').unwrap_or(false);
    if ok_word || ok_num {
        if let Ok(v) = t.parse::<f64>() {
            return Ok(v);
        }
    }
    Err(Error::value_error(format!("could not convert string to float: {}", repr_str(text))))
}

/// `int(text)` for a string.
pub fn parse_int(text: &str) -> Result<i64> {
    let t = strip(text).replace('_', "");
    t.parse::<i64>()
        .map_err(|_| Error::value_error(format!("invalid literal for int() with base 10: {}", repr_str(text))))
}

/// `float(value)` for a JSON value.
pub fn to_float(v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => parse_float(s),
        other => Err(Error::type_error(format!(
            "float() argument must be a string or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

/// `int(value)` for a JSON value.
pub fn to_int(v: &Value) -> Result<i64> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_infinite() {
                    Err(Error::py("OverflowError", "cannot convert float infinity to integer"))
                } else {
                    Ok(f.trunc() as i64)
                }
            }
        }
        Value::Bool(b) => Ok(*b as i64),
        Value::String(s) => parse_int(s),
        other => Err(Error::type_error(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

/// A number as f64 when the value is a Python int, float or bool.
pub fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(*b as i64 as f64),
        _ => None,
    }
}

pub fn is_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_i64() || n.is_u64())
}

pub fn float_value(x: f64) -> Value {
    Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}

// -- values -------------------------------------------------------------------

pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python truthiness.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `==` (numbers and bools compare by value).
pub fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(_) | Value::Bool(_), Value::Number(_) | Value::Bool(_)) => {
            match (a, b) {
                (Value::Number(x), Value::Number(y)) if !x.is_f64() && !y.is_f64() => x == y,
                _ => as_number(a) == as_number(b),
            }
        }
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).map(|w| eq(v, w)).unwrap_or(false))
        }
        _ => a == b,
    }
}

pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) || c == '\u{ad}' => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr(value)`.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => number_str(n),
        Value::String(s) => repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", repr_str(k), repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str(value)`.
pub fn str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

fn number_str(n: &Number) -> String {
    if n.is_f64() {
        float_repr(n.as_f64().unwrap())
    } else {
        n.to_string()
    }
}

// -- strings ------------------------------------------------------------------

/// `str.isspace()` for one character.
pub fn is_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | '\u{1c}'..='\u{1f}' | ' ' | '\u{85}' | '\u{a0}'
        | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.split()`.
pub fn split(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// `str.strip().upper()`.
pub fn strip_upper(s: &str) -> String {
    strip(s).to_uppercase()
}

/// `str.title()`.
pub fn title(s: &str) -> String {
    let mut out = String::new();
    let mut prev_cased = false;
    for c in s.chars() {
        let cased = c.is_lowercase() || c.is_uppercase();
        if prev_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        prev_cased = cased;
    }
    out
}

/// Text read through Python's universal newlines.
pub fn universal_newlines(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}

/// `open(path).read()` in text mode.
pub fn read_text(path: &std::path::Path, shown: &str) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(&e, shown))?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(universal_newlines(text)),
        Err(e) => {
            let pos = e.utf8_error().valid_up_to();
            let byte = e.as_bytes()[pos];
            Err(Error::py(
                "UnicodeDecodeError",
                format!("'utf-8' codec can't decode byte 0x{:02x} in position {}: invalid start byte", byte, pos),
            ))
        }
    }
}

/// Python `str.splitlines(keepends=True)` restricted to `\n` (text already
/// went through universal newlines), as `for line in f` yields.
pub fn file_lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

// -- json.dumps -----------------------------------------------------------------

fn dump_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e && (c as u32) != 0x7f => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn dump_float(x: f64) -> String {
    if x.is_nan() {
        "NaN".into()
    } else if x.is_infinite() {
        if x > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        float_repr(x)
    }
}

fn dump_value(v: &Value, indent: Option<usize>, level: usize, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&dump_float(n.as_f64().unwrap()))
            } else {
                out.push_str(&n.to_string())
            }
        }
        Value::String(s) => dump_str(s, out),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(indent, level + 1, out);
                dump_value(item, indent, level + 1, out);
            }
            newline(indent, level, out);
            out.push(']');
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if indent.is_none() {
                        out.push(' ');
                    }
                }
                newline(indent, level + 1, out);
                dump_str(k, out);
                out.push_str(": ");
                dump_value(&o[k.as_str()], indent, level + 1, out);
            }
            newline(indent, level, out);
            out.push('}');
        }
    }
}

fn newline(indent: Option<usize>, level: usize, out: &mut String) {
    if let Some(n) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(n * level));
    }
}

/// `json.dumps(v, indent=2, sort_keys=True)`.
pub fn dumps_indent(v: &Value) -> String {
    let mut out = String::new();
    dump_value(v, Some(2), 0, &mut out);
    out
}

/// `json.dumps(v, sort_keys=True)`.
pub fn dumps_compact(v: &Value) -> String {
    let mut out = String::new();
    dump_value(v, None, 0, &mut out);
    out
}

// -- json.loads -----------------------------------------------------------------

/// `json.JSONDecodeError`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

struct Parser {
    s: Vec<char>,
}

enum Fail {
    Stop(usize),
    Msg(&'static str, usize),
    Unsupported(usize),
}

impl Parser {
    fn ws(&self, mut i: usize) -> usize {
        while i < self.s.len() && matches!(self.s[i], ' ' | '\t' | '\n' | '\r') {
            i += 1;
        }
        i
    }

    fn lit(&self, i: usize, word: &str) -> bool {
        let w: Vec<char> = word.chars().collect();
        i + w.len() <= self.s.len() && self.s[i..i + w.len()] == w[..]
    }

    fn scan(&self, i: usize) -> std::result::Result<(Value, usize), Fail> {
        if i >= self.s.len() {
            return Err(Fail::Stop(i));
        }
        match self.s[i] {
            '"' => {
                let (s, end) = self.string(i + 1)?;
                Ok((Value::String(s), end))
            }
            '{' => self.object(i + 1),
            '[' => self.array(i + 1),
            'n' if self.lit(i, "null") => Ok((Value::Null, i + 4)),
            't' if self.lit(i, "true") => Ok((Value::Bool(true), i + 4)),
            'f' if self.lit(i, "false") => Ok((Value::Bool(false), i + 5)),
            'N' if self.lit(i, "NaN") => Err(Fail::Unsupported(i)),
            'I' if self.lit(i, "Infinity") => Err(Fail::Unsupported(i)),
            '-' if self.lit(i, "-Infinity") => Err(Fail::Unsupported(i)),
            _ => self.number(i),
        }
    }

    fn number(&self, start: usize) -> std::result::Result<(Value, usize), Fail> {
        let s = &self.s;
        let n = s.len();
        let mut i = start;
        if i < n && s[i] == '-' {
            i += 1;
        }
        if i < n && ('1'..='9').contains(&s[i]) {
            i += 1;
            while i < n && s[i].is_ascii_digit() {
                i += 1;
            }
        } else if i < n && s[i] == '0' {
            i += 1;
        } else {
            return Err(Fail::Stop(start));
        }
        let mut is_float = false;
        if i + 1 < n && s[i] == '.' && s[i + 1].is_ascii_digit() {
            is_float = true;
            i += 2;
            while i < n && s[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i < n && (s[i] == 'e' || s[i] == 'E') {
            let e_start = i;
            i += 1;
            if i < n && (s[i] == '-' || s[i] == '+') {
                i += 1;
            }
            let digits = i;
            while i < n && s[i].is_ascii_digit() {
                i += 1;
            }
            if i > digits {
                is_float = true;
            } else {
                i = e_start;
            }
        }
        let text: String = s[start..i].iter().collect();
        if is_float {
            let f: f64 = text.parse().unwrap();
            match Number::from_f64(f) {
                Some(num) if f.is_finite() => Ok((Value::Number(num), i)),
                _ => Err(Fail::Unsupported(start)),
            }
        } else if let Ok(v) = text.parse::<i64>() {
            Ok((Value::Number(v.into()), i))
        } else if let Ok(v) = text.parse::<u64>() {
            Ok((Value::Number(v.into()), i))
        } else {
            Err(Fail::Unsupported(start))
        }
    }

    fn string(&self, begin: usize) -> std::result::Result<(String, usize), Fail> {
        let s = &self.s;
        let n = s.len();
        let quote = begin - 1;
        let mut out = String::new();
        let mut i = begin;
        loop {
            while i < n && s[i] != '"' && s[i] != '\\' {
                if (s[i] as u32) <= 0x1f {
                    return Err(Fail::Msg("Invalid control character at", i));
                }
                out.push(s[i]);
                i += 1;
            }
            if i == n {
                return Err(Fail::Msg("Unterminated string starting at", quote));
            }
            if s[i] == '"' {
                return Ok((out, i + 1));
            }
            i += 1;
            if i == n {
                return Err(Fail::Msg("Unterminated string starting at", quote));
            }
            let c = s[i];
            if c != 'u' {
                let mapped = match c {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{08}',
                    'f' => '\u{0c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err(Fail::Msg("Invalid \\escape", i - 1)),
                };
                out.push(mapped);
                i += 1;
                continue;
            }
            let next = i + 1;
            let end = next + 4;
            if end >= n {
                return Err(Fail::Msg("Invalid \\uXXXX escape", next - 1));
            }
            let mut code = 0u32;
            for &d in &s[next..end] {
                code = (code << 4) | d.to_digit(16).ok_or(Fail::Msg("Invalid \\uXXXX escape", end - 5))?;
            }
            i = end;
            if (0xd800..0xdc00).contains(&code) && end + 6 < n && s[end] == '\\' && s[end + 1] == 'u' {
                let end2 = end + 6;
                let mut c2 = 0u32;
                for &d in &s[end + 2..end2] {
                    c2 = (c2 << 4) | d.to_digit(16).ok_or(Fail::Msg("Invalid \\uXXXX escape", end2 - 5))?;
                }
                if (0xdc00..0xe000).contains(&c2) {
                    code = 0x10000 + ((code - 0xd800) << 10) + (c2 - 0xdc00);
                    i = end2;
                }
            }
            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
        }
    }

    fn object(&self, start: usize) -> std::result::Result<(Value, usize), Fail> {
        let s = &self.s;
        let n = s.len();
        let mut map = Map::new();
        let mut i = self.ws(start);
        if i < n && s[i] == '}' {
            return Ok((Value::Object(map), i + 1));
        }
        loop {
            if i >= n || s[i] != '"' {
                return Err(Fail::Msg("Expecting property name enclosed in double quotes", i));
            }
            let (key, next) = self.string(i + 1)?;
            i = self.ws(next);
            if i >= n || s[i] != ':' {
                return Err(Fail::Msg("Expecting ':' delimiter", i));
            }
            i = self.ws(i + 1);
            let (value, next) = self.scan(i)?;
            map.insert(key, value);
            i = self.ws(next);
            if i < n && s[i] == '}' {
                return Ok((Value::Object(map), i + 1));
            }
            if i >= n || s[i] != ',' {
                return Err(Fail::Msg("Expecting ',' delimiter", i));
            }
            let comma = i;
            i = self.ws(i + 1);
            if i < n && s[i] == '}' {
                return Err(Fail::Msg("Illegal trailing comma before end of object", comma));
            }
        }
    }

    fn array(&self, start: usize) -> std::result::Result<(Value, usize), Fail> {
        let s = &self.s;
        let n = s.len();
        let mut items = Vec::new();
        let mut i = self.ws(start);
        if i < n && s[i] == ']' {
            return Ok((Value::Array(items), i + 1));
        }
        loop {
            let (value, next) = self.scan(i)?;
            items.push(value);
            i = self.ws(next);
            if i < n && s[i] == ']' {
                return Ok((Value::Array(items), i + 1));
            }
            if i >= n || s[i] != ',' {
                return Err(Fail::Msg("Expecting ',' delimiter", i));
            }
            let comma = i;
            i = self.ws(i + 1);
            if i < n && s[i] == ']' {
                return Err(Fail::Msg("Illegal trailing comma before end of array", comma));
            }
        }
    }

    fn message(&self, msg: &str, pos: usize) -> DecodeError {
        let before = &self.s[..pos.min(self.s.len())];
        let line = before.iter().filter(|c| **c == '\n').count() + 1;
        let col = match before.iter().rposition(|c| *c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        DecodeError(format!("{}: line {} column {} (char {})", msg, line, col, pos))
    }
}

/// `json.loads(text)`. NaN, Infinity and numbers that do not fit a double or
/// a 64-bit integer cannot be held in a `serde_json::Value` and are refused.
pub fn loads(text: &str) -> std::result::Result<Value, DecodeError> {
    let p = Parser { s: text.chars().collect() };
    if p.s.first() == Some(&'\u{feff}') {
        return Err(p.message("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    let start = p.ws(0);
    match p.scan(start) {
        Ok((v, end)) => {
            let end = p.ws(end);
            if end != p.s.len() {
                return Err(p.message("Extra data", end));
            }
            Ok(v)
        }
        Err(Fail::Stop(i)) => Err(p.message("Expecting value", i)),
        Err(Fail::Msg(m, i)) => Err(p.message(m, i)),
        Err(Fail::Unsupported(i)) => Err(p.message("Unsupported number (jobcore)", i)),
    }
}

// -- sha256 ---------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 of `data` as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{:08x}", x)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_format_like_python() {
        assert_eq!(float_repr(3200.0), "3200.0");
        assert_eq!(float_repr(0.1), "0.1");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1e-5), "1e-05");
        assert_eq!(float_repr(1e-4), "0.0001");
        assert_eq!(float_repr(1e15), "1000000000000000.0");
        assert_eq!(float_repr(-2.5), "-2.5");
        assert_eq!(float_repr(6.35 / 25.4), "0.25");
        assert_eq!(general(1234567.0), "1.23457e+06");
        assert_eq!(general(4250.0), "4250");
        assert_eq!(general(2.0), "2");
        assert_eq!(general(0.0001), "0.0001");
        assert_eq!(general(0.00001), "1e-05");
        assert_eq!(fixed(-0.0, 4), "-0.0000");
        assert_eq!(fixed(-0.00001, 4), "-0.0000");
        assert_eq!(fixed(2.0005, 3), "2.001");
        assert_eq!(fixed(0.25, 1), "0.2");
        assert_eq!(round1(92.15), 92.2);
    }

    #[test]
    fn json_matches_python() {
        let v = loads(r#"{"b": [1, 2.0, {}], "a": "xé", "c": []}"#).unwrap();
        assert_eq!(dumps_compact(&v), r#"{"a": "x\u00e9", "b": [1, 2.0, {}], "c": []}"#);
        assert_eq!(dumps_indent(&v), "{\n  \"a\": \"x\\u00e9\",\n  \"b\": [\n    1,\n    2.0,\n    {}\n  ],\n  \"c\": []\n}");
        let cases = [
            ("", "Expecting value: line 1 column 1 (char 0)"),
            ("{", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ("{\"a\":1,}", "Illegal trailing comma before end of object: line 1 column 7 (char 6)"),
            ("[1,\n2,]", "Illegal trailing comma before end of array: line 2 column 2 (char 5)"),
            ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
            ("{\"a\" 1}", "Expecting ':' delimiter: line 1 column 6 (char 5)"),
            ("\"abc", "Unterminated string starting at: line 1 column 1 (char 0)"),
            ("\"a\\q\"", "Invalid \\escape: line 1 column 3 (char 2)"),
            ("\"a\\u12\"", "Invalid \\uXXXX escape: line 1 column 4 (char 3)"),
            ("01", "Extra data: line 1 column 2 (char 1)"),
            ("[-]", "Expecting value: line 1 column 2 (char 1)"),
            ("\"a\tb\"", "Invalid control character at: line 1 column 3 (char 2)"),
            ("1e", "Extra data: line 1 column 2 (char 1)"),
        ];
        let mut bad = Vec::new();
        for (text, msg) in cases {
            let got = loads(text).map(|v| v.to_string()).unwrap_or_else(|e| e.0);
            if got != msg {
                bad.push(format!("{:?}: {}", text, got));
            }
        }
        assert!(bad.is_empty(), "{:?}", bad);
    }

    #[test]
    fn sha256_known_value() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_hex(&[b'a'; 1000]).len(), 64);
    }

    #[test]
    fn strings_like_python() {
        assert_eq!(title("RIGID TAP"), "Rigid Tap");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("abc"), "'abc'");
    }
}
