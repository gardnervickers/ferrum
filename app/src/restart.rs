//! RUN FROM SELECTED: start a plain program part way through.
//!
//! LinuxCNC's own run-from-line (EMC_TASK_PLAN_RUN with a line) reads the
//! lines before the start so the interpreter's modes, offsets and tool
//! length offset are right, but it throws away what those lines would have
//! done to the machine: tool changes, spindle and coolant never happen. So
//! before starting, millgui:
//!
//! - works out from the skipped lines which tool, spindle and coolant the
//!   start line expects (`plan`);
//! - refuses when the wrong tool is in the spindle, when the line is inside
//!   an o-word block or with cutter compensation on, or for a JOB program
//!   (jobs start at an op on the JOB screen instead);
//! - raises Z to machine top (G53 G0 Z0), then starts the spindle and
//!   coolant the line expects, then runs from the line. The first move of
//!   the program then starts from the top of Z.

use linuxcnc::{Command, Handle, Interp, Status};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// What the program expects at the start line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    /// 0-based index of the start line.
    pub index: usize,
    /// Tool loaded by the last M6 (or M61) before the line; None if none.
    pub tool: Option<i32>,
    /// The start line changes the tool itself, so whatever is loaded is fine.
    pub changes_tool: bool,
    /// 1 = M3, -1 = M4, 0 = off.
    pub spindle: i8,
    pub rpm: f64,
    pub mist: bool,
    pub flood: bool,
    /// The last comment-only line before (or at) the start: the CAM op name.
    pub op: Option<String>,
    /// Why this line cannot be a start line.
    pub problem: Option<String>,
}

/// Code words of one line, comments removed: ('G', 0.0), ('X', 1.5)...
fn words(line: &str) -> Vec<(char, f64)> {
    let mut code = String::new();
    let mut depth = 0;
    for c in line.chars() {
        match c {
            ';' if depth == 0 => break,
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => code.push(c.to_ascii_uppercase()),
            _ => {}
        }
    }
    // Expressions and parameters ([...], #) are not evaluated: a word whose
    // value is not a plain number is skipped.
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() {
            let mut j = i + 1;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let s = j;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.' || ((b[j] == b'-' || b[j] == b'+') && j == s)) {
                j += 1;
            }
            if let Ok(v) = code[s..j].parse::<f64>() {
                out.push((c, v));
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// The o-word keyword of a line, lower case ("sub", "while", "endif"...).
fn oword(line: &str) -> Option<String> {
    let t = line.trim_start();
    let t = t.strip_prefix(['o', 'O'])?;
    let rest = t.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '<' || c == '>' || c == '_' || c == '#');
    rest.split_whitespace().next().map(|w| w.to_ascii_lowercase())
}

fn comment_only(line: &str) -> Option<String> {
    let t = line.trim();
    let inner = t.strip_prefix('(')?.strip_suffix(')')?;
    (!inner.contains(['(', ')']) && !inner.trim().is_empty()).then(|| inner.trim().to_string())
}

pub fn plan<S: AsRef<str>>(lines: &[S], index: usize) -> Plan {
    let mut p = Plan { index, ..Default::default() };
    let mut selected: Option<i32> = None;
    let mut depth: i32 = 0;
    let mut comp = false;
    for raw in lines.iter().take(index) {
        let line = raw.as_ref();
        if let Some(c) = comment_only(line) {
            p.op = Some(c);
            continue;
        }
        if let Some(k) = oword(line) {
            match k.as_str() {
                "sub" | "while" | "if" | "do" | "repeat" => depth += 1,
                "endsub" | "endwhile" | "endif" | "endrepeat" => depth -= 1,
                // `do ... while`: the closing while ends the block.
                _ => {}
            }
            continue;
        }
        let w = words(line);
        let has = |l: char, v: f64| w.iter().any(|&(a, b)| a == l && (b - v).abs() < 1e-6);
        if let Some(&(_, t)) = w.iter().find(|(a, _)| *a == 'T') {
            selected = Some(t as i32);
        }
        if has('M', 6.0) {
            p.tool = selected;
        }
        if has('M', 61.0) {
            p.tool = w.iter().find(|(a, _)| *a == 'Q').map(|&(_, q)| q as i32);
        }
        if let Some(&(_, s)) = w.iter().find(|(a, _)| *a == 'S') {
            p.rpm = s;
        }
        if has('M', 3.0) {
            p.spindle = 1;
        }
        if has('M', 4.0) {
            p.spindle = -1;
        }
        if has('M', 5.0) || has('M', 2.0) || has('M', 30.0) {
            p.spindle = 0;
        }
        if has('M', 7.0) {
            p.mist = true;
        }
        if has('M', 8.0) {
            p.flood = true;
        }
        if has('M', 9.0) {
            p.mist = false;
            p.flood = false;
        }
        if has('G', 40.0) {
            comp = false;
        }
        if has('G', 41.0) || has('G', 42.0) || has('G', 41.1) || has('G', 42.1) {
            comp = true;
        }
    }
    if let Some(at) = lines.get(index).map(AsRef::as_ref) {
        if let Some(c) = comment_only(at) {
            p.op = Some(c);
        }
        let w = words(at);
        p.changes_tool = w.iter().any(|&(a, b)| a == 'M' && (b == 6.0 || b == 61.0));
        if oword(at).is_some() {
            p.problem = Some("Pick a line outside o-word blocks".into());
        }
    } else {
        p.problem = Some("Pick a line of the program".into());
    }
    if depth > 0 {
        p.problem = Some("This line is inside a subroutine, loop or if block".into());
    } else if comp {
        p.problem = Some("Cutter compensation is on at this line; pick a line before the G41/G42".into());
    }
    p
}

impl Plan {
    /// One line for under the program: what the start will expect and do.
    pub fn summary(&self, loaded: i32) -> String {
        if let Some(e) = &self.problem {
            return e.clone();
        }
        let mut parts = Vec::new();
        if let Some(op) = &self.op {
            parts.push(op.clone());
        }
        match (self.changes_tool, self.tool) {
            (true, _) => parts.push("changes tool on this line".into()),
            (false, Some(t)) if t != loaded => parts.push(format!("needs T{t} (T{loaded} loaded)")),
            (false, Some(t)) => parts.push(format!("T{t}")),
            (false, None) => {}
        }
        parts.push(match self.spindle {
            1 => format!("spindle CW {:.0}", self.rpm),
            -1 => format!("spindle CCW {:.0}", self.rpm),
            _ => "spindle off".into(),
        });
        if self.mist {
            parts.push("mist".into());
        }
        if self.flood {
            parts.push("flood".into());
        }
        format!("Line {}: {}. Z goes to the top first.", self.index + 1, parts.join(", "))
    }

    /// Why the start must be refused with this status, if it must.
    pub fn refuse(&self, s: &Status) -> Option<String> {
        if let Some(e) = &self.problem {
            return Some(e.clone());
        }
        if s.interp != Interp::Idle {
            return Some("Stop the program first".into());
        }
        if !s.power_on() || !s.all_homed() {
            return Some("Turn the machine on and home it first".into());
        }
        if !self.changes_tool {
            if let Some(t) = self.tool {
                if t != s.tool_in_spindle {
                    return Some(format!("Load T{t} first (this line expects it; T{} is loaded)", s.tool_in_spindle));
                }
            }
        }
        if self.spindle != 0 && self.rpm <= 0.0 {
            return Some("No spindle speed (S) before this line".into());
        }
        None
    }

    /// The MDI lines to send before the run, in order.
    pub fn prelude(&self) -> Vec<String> {
        let mut v = vec!["G53 G0 Z0".to_string()];
        if self.spindle != 0 && !self.changes_tool {
            v.push(format!("{} S{:.0}", if self.spindle > 0 { "M3" } else { "M4" }, self.rpm));
        }
        if self.mist {
            v.push("M7".into());
        }
        if self.flood {
            v.push("M8".into());
        }
        v
    }
}

/// Sends the prelude one MDI line at a time, each once the previous one
/// finished, then runs from the line. Any error or STOP ends it.
pub struct Restart {
    steps: VecDeque<String>,
    line: i32,
    sent: Option<(Instant, i32, bool)>,
}

impl Restart {
    pub fn new(plan: &Plan) -> Self {
        Self { steps: plan.prelude().into(), line: plan.index as i32 + 1, sent: None }
    }

    /// Advance on each status. Returns false once finished (started or failed).
    pub fn pump(&mut self, s: &Status, m: &Handle) -> bool {
        if let Some((at, serial, seen_busy)) = &mut self.sent {
            if s.busy() {
                *seen_busy = true;
                return true;
            }
            let echoed = s.echo_serial > *serial;
            if !(echoed && (*seen_busy || at.elapsed() > Duration::from_millis(300))) {
                return at.elapsed() < Duration::from_secs(5);
            }
            self.sent = None;
        }
        if let Some(line) = self.steps.pop_front() {
            m.send(Command::Mdi(line));
            self.sent = Some((Instant::now(), s.echo_serial, false));
            return true;
        }
        m.send(Command::SetMode(linuxcnc::Mode::Auto));
        m.send(Command::ProgramRun(self.line));
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FUSION: &[&str] = &[
        "%",
        "(1001)",
        "G90 G94 G17 G91.1",
        "G21",
        "(2D ADAPTIVE1)",
        "T3 M6",
        "S8000 M3",
        "G54",
        "M7",
        "G0 X10 Y10",
        "G43 Z15 H3",
        "G1 Z-1 F300",
        "(2D CONTOUR1)",
        "G0 Z15",
        "G0 X0 Y0",
        "(DRILL1)",
        "M9",
        "M5",
        "T5 M6",
        "S2000 M3",
        "G0 X5 Y5",
    ];

    #[test]
    fn plan_tracks_tool_spindle_coolant_and_op() {
        let p = plan(FUSION, 13);
        assert_eq!(p.tool, Some(3));
        assert_eq!((p.spindle, p.rpm, p.mist, p.flood), (1, 8000.0, true, false));
        assert_eq!(p.op.as_deref(), Some("2D CONTOUR1"));
        assert!(!p.changes_tool && p.problem.is_none());
        assert_eq!(p.prelude(), vec!["G53 G0 Z0", "M3 S8000", "M7"]);
        // At the T5 M6 line: the line changes the tool, no spindle start.
        let p = plan(FUSION, 18);
        assert!(p.changes_tool);
        assert_eq!((p.tool, p.spindle, p.mist), (Some(3), 0, false));
        assert_eq!(p.prelude(), vec!["G53 G0 Z0"]);
        let p = plan(FUSION, 20);
        assert_eq!((p.tool, p.rpm, p.spindle), (Some(5), 2000.0, 1));
        assert_eq!(p.op.as_deref(), Some("DRILL1"));
    }

    #[test]
    fn plan_refuses_blocks_and_comp() {
        let g = ["o100 sub", "G0 X1", "o100 endsub", "o200 while [#1 LT 3]", "G1 X2", "o200 endwhile", "G41 D3", "G1 X3", "G40", "G1 X4"];
        assert!(plan(&g, 1).problem.is_some());
        assert!(plan(&g, 3).problem.is_some(), "o-word line itself");
        assert!(plan(&g, 4).problem.is_some());
        assert!(plan(&g, 7).problem.is_some(), "comp on");
        assert!(plan(&g, 9).problem.is_none());
    }

    #[test]
    fn words_parse_compact_and_comments() {
        assert_eq!(words("M3S1200(spindle on)"), vec![('M', 3.0), ('S', 1200.0)]);
        assert_eq!(words("g1 x-1.5 ; note M5"), vec![('G', 1.0), ('X', -1.5)]);
        assert_eq!(words("T#1 M6"), vec![('M', 6.0)]);
    }

    #[test]
    fn summary_reads_well() {
        let p = plan(FUSION, 13);
        assert_eq!(p.summary(3), "Line 14: 2D CONTOUR1, T3, spindle CW 8000, mist. Z goes to the top first.");
        assert!(p.summary(1).contains("needs T3 (T1 loaded)"));
    }
}
