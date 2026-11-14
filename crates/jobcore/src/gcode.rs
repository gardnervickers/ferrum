//! Small G-code helpers for checking op bodies (`mymill_job/gcode.py`).

use crate::py;
use regex::Regex;
use std::sync::OnceLock;

/// Codes a body may never contain, with the reason shown to the operator.
pub const FORBIDDEN: [(char, f64, &str); 20] = [
    ('M', 2.0, "ends the program"),
    ('M', 30.0, "ends the program"),
    ('M', 60.0, "is a pallet change"),
    ('M', 6.0, "changes tool; the job does tool changes itself"),
    ('M', 61.0, "sets the loaded tool; the job does tool changes itself"),
    ('G', 10.0, "writes offsets or tool data that later ops rely on"),
    ('G', 92.0, "shifts every work offset"),
    ('G', 92.1, "clears G92 offsets"),
    ('G', 92.2, "suspends G92 offsets"),
    ('G', 92.3, "restores G92 offsets"),
    ('G', 28.1, "stores a reference position"),
    ('G', 30.1, "stores a reference position"),
    ('G', 49.0, "cancels the tool length offset"),
    ('G', 43.1, "applies a dynamic tool length offset"),
    ('G', 43.2, "adds a tool length offset"),
    ('G', 38.2, "is a probing move"),
    ('G', 38.3, "is a probing move"),
    ('G', 38.4, "is a probing move"),
    ('G', 38.5, "is a probing move"),
    ('G', 33.1, "needs spindle feedback (rigid tap)"),
];

pub fn forbidden(letter: char, value: f64) -> Option<&'static str> {
    FORBIDDEN.iter().find(|(l, v, _)| *l == letter && *v == value).map(|(_, _, why)| *why)
}

pub fn units_code(value: f64) -> Option<&'static str> {
    if value == 20.0 {
        Some("IN")
    } else if value == 21.0 {
        Some("MM")
    } else {
        None
    }
}

pub fn wcs_code(value: f64) -> Option<&'static str> {
    [(54.0, "G54"), (55.0, "G55"), (56.0, "G56"), (57.0, "G57"), (58.0, "G58"), (59.0, "G59"),
     (59.1, "G59.1"), (59.2, "G59.2"), (59.3, "G59.3")]
        .iter()
        .find(|(v, _)| *v == value)
        .map(|(_, c)| *c)
}

struct Patterns {
    paren: Regex,
    oword: Regex,
    assignment: Regex,
    any_word: Regex,
    slash: Regex,
    line_number: Regex,
    o_start: Regex,
    unsafe_comment: Regex,
    spaces: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| {
        let value = r"(?:\x00|#\s*(?:<[^>]*>|\d+)|[-+]?(?:\d+\.?\d*|\.\d+))";
        Patterns {
            paren: Regex::new(r"\([^)]*\)").unwrap(),
            oword: Regex::new(r"(?i)^O\s*(<[^>]*>|\d+)\s*([A-Z]+)?").unwrap(),
            assignment: Regex::new(&format!(r"#\s*(?:<[^>]*>|\d+)\s*=\s*{}", value)).unwrap(),
            any_word: Regex::new(&format!(r"([A-Z])\s*({})", value)).unwrap(),
            slash: Regex::new(r"^\s*/").unwrap(),
            line_number: Regex::new(r"^\s*N\s*\d+").unwrap(),
            o_start: Regex::new(r"^\s*O").unwrap(),
            unsafe_comment: Regex::new(r"[^A-Za-z0-9 ._,=:/+-]").unwrap(),
            spaces: Regex::new(r"\s+").unwrap(),
        }
    })
}

pub fn strip_comment(line: &str) -> String {
    let text = patterns().paren.replace_all(line, " ");
    text.split(';').next().unwrap_or("").to_string()
}

fn drop_expressions(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in text.chars() {
        if ch == '[' {
            if depth == 0 {
                out.push('\x00');
            }
            depth += 1;
        } else if ch == ']' {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            out.push(ch);
        }
    }
    out
}

/// `[(letter, number), ...]` for one line; `None` for a computed value.
pub fn words(line: &str) -> Vec<(char, Option<f64>)> {
    let p = patterns();
    let text = strip_comment(line).to_uppercase();
    let text = p.slash.replace(&text, "").into_owned();
    let text = p.line_number.replace(&text, "").into_owned();
    if p.o_start.is_match(&text) {
        return vec![];
    }
    let text = drop_expressions(&text);
    let text = p.assignment.replace_all(&text, " ").into_owned();
    let mut result = Vec::new();
    for cap in p.any_word.captures_iter(&text) {
        let letter = cap[1].chars().next().unwrap();
        let number = &cap[2];
        if number == "\x00" || number.starts_with('#') {
            result.push((letter, None));
            continue;
        }
        let mut value: f64 = number.parse().unwrap_or(0.0);
        if letter == 'G' || letter == 'M' {
            value = py::round1(value);
        }
        result.push((letter, Some(value)));
    }
    result
}

/// `(label, keyword)` for an O-word line.
pub fn oword(line: &str) -> Option<(String, String)> {
    let text = strip_comment(line);
    let text = py::strip(&text);
    let cap = patterns().oword.captures(text)?;
    Some((
        cap[1].to_lowercase(),
        cap.get(2).map(|m| m.as_str().to_lowercase()).unwrap_or_default(),
    ))
}

/// Make text safe to place inside a G-code comment.
pub fn comment_text(text: &str) -> String {
    let p = patterns();
    let t = p.unsafe_comment.replace_all(text, " ");
    let t = p.spaces.replace_all(&t, " ");
    py::strip(&t).to_uppercase()
}

/// `fmt`: four places, trailing zeros removed.
pub fn fmt(value: f64) -> String {
    let text = py::fixed(value, 4);
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    };
    if text == "-0" || text.is_empty() {
        "0".into()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_words() {
        assert_eq!(words("G59.10 X1"), vec![('G', Some(59.1)), ('X', Some(1.0))]);
        assert_eq!(words("#1=2 M30"), vec![('M', Some(30.0))]);
        assert_eq!(words("S[#1*2] M3"), vec![('S', None), ('M', Some(3.0))]);
        assert_eq!(words("/M30"), vec![('M', Some(30.0))]);
        assert_eq!(words("o100 if [1]"), vec![]);
        assert_eq!(words("(T5 M6 in a comment)"), vec![]);
        assert_eq!(oword("o<job_op1> if [1]"), Some(("<job_op1>".into(), "if".into())));
        assert_eq!(comment_text("close) M30 ("), "CLOSE M30");
        assert_eq!(fmt(4.2), "4.2");
        assert_eq!(fmt(-0.00001), "0");
        assert_eq!(fmt(100.0), "100");
    }
}
