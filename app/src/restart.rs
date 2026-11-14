//! RUN FROM SELECTED: start a plain program part way through.
//!
//! LinuxCNC's own run-from-line (EMC_TASK_PLAN_RUN with a line) is not used.
//! It only throws away what the skipped lines do at the program's top level:
//! a remapped M6 before the start line runs its subroutine for real (the
//! tool change dialog, M5, M61...), and the spindle ends up off when the
//! program starts moving. Instead millgui writes a restart copy of the
//! program and runs that from its first line:
//!
//! - the lines before the start line are replaced by a short preamble that
//!   sets what they left behind (units, plane, work offset, tool length
//!   offset, feed, spindle, coolant) and moves to where the start line
//!   begins: Z to machine top, X and Y over the spot, rapid down to the last
//!   rapid height above it, then feed down at the program's feed;
//! - the rest is padded with blank lines so every line keeps its number, and
//!   the screen keeps showing the original program;
//! - the original is loaded again when the run ends or stops.
//!
//! It refuses when the wrong tool is in the spindle, when the line is in or
//! after o-word code or parameters, inside a drilling cycle, with cutter
//! compensation or incremental moves on, or for a JOB program (jobs start at
//! an op on the JOB screen instead).

use linuxcnc::{Command, Handle, Interp, Status};
use std::path::{Path, PathBuf};
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
    /// Modal words in force at the start line: units, plane, feed mode, arc
    /// distance, work offset ("G21", "G17"...), in that order.
    modal: Vec<String>,
    /// "G43 H3", "G43" or "G49" (None: never set).
    tlo: Option<String>,
    feed: Option<f64>,
    /// Motion mode in force: 0, 1, 2, 3.
    motion: Option<u8>,
    /// Program position before the start line, per axis (None: unknown,
    /// e.g. after a G53 move).
    pos: [Option<f64>; 3],
    /// Height of the last rapid that only moved Z: the retract plane.
    rapid_z: Option<f64>,
    /// The first line starts with "%", so the copy must too.
    percent: bool,
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

/// The line has code that is not a plain number word: o-words, parameters
/// or expressions.
fn not_plain(line: &str) -> bool {
    let mut depth = 0;
    let mut code = String::new();
    for c in line.chars() {
        match c {
            ';' if depth == 0 => break,
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => code.push(c),
            _ => {}
        }
    }
    let t = code.trim_start();
    t.starts_with(['o', 'O']) || code.contains(['#', '[', '<'])
}

fn comment_only(line: &str) -> Option<String> {
    let t = line.trim();
    let inner = t.strip_prefix('(')?.strip_suffix(')')?;
    (!inner.contains(['(', ')']) && !inner.trim().is_empty()).then(|| inner.trim().to_string())
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

pub fn plan<S: AsRef<str>>(lines: &[S], index: usize) -> Plan {
    let mut p = Plan { index, ..Default::default() };
    let mut selected: Option<i32> = None;
    let mut comp = false;
    let mut incremental = false;
    let mut cycle = false;
    let mut code_before = false;
    // units, plane, feed mode, arc distance, work offset
    let mut modal: [Option<String>; 5] = Default::default();
    p.percent = lines.first().is_some_and(|l| l.as_ref().trim() == "%");
    for raw in lines.iter().take(index) {
        let line = raw.as_ref();
        if let Some(c) = comment_only(line) {
            p.op = Some(c);
            continue;
        }
        if not_plain(line) {
            code_before = true;
            continue;
        }
        let w = words(line);
        let has = |l: char, v: f64| w.iter().any(|&(a, b)| a == l && (b - v).abs() < 1e-6);
        let gs: Vec<f64> = w.iter().filter(|(a, _)| *a == 'G').map(|&(_, v)| v).collect();
        let g = |v: f64| gs.iter().any(|&b| (b - v).abs() < 1e-6);
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
        if let Some(&(_, f)) = w.iter().find(|(a, _)| *a == 'F') {
            p.feed = Some(f);
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
        for &v in &gs {
            let slot = match v {
                _ if v == 20.0 || v == 21.0 => Some(0),
                _ if v == 17.0 || v == 18.0 || v == 19.0 => Some(1),
                _ if v == 93.0 || v == 94.0 || v == 95.0 => Some(2),
                _ if (v - 90.1).abs() < 1e-6 || (v - 91.1).abs() < 1e-6 => Some(3),
                _ if (54.0..=59.3).contains(&v) => Some(4),
                _ => None,
            };
            if let Some(i) = slot {
                modal[i] = Some(format!("G{}", fmt(v)));
            }
        }
        if g(90.0) {
            incremental = false;
        }
        if g(91.0) {
            incremental = true;
        }
        if g(40.0) {
            comp = false;
        }
        if g(41.0) || g(42.0) || g(41.1) || g(42.1) {
            comp = true;
        }
        if g(49.0) {
            p.tlo = Some("G49".into());
        }
        if g(43.0) {
            p.tlo = Some(match w.iter().find(|(a, _)| *a == 'H') {
                Some(&(_, h)) => format!("G43 H{}", h as i32),
                None => "G43".into(),
            });
        }
        if g(80.0) {
            cycle = false;
        }
        if gs.iter().any(|&v| (81.0..=89.0).contains(&v)) {
            cycle = true;
        }
        for &v in &gs {
            if v == 0.0 || v == 1.0 || v == 2.0 || v == 3.0 {
                p.motion = Some(v as u8);
            }
        }
        let axes = ['X', 'Y', 'Z'];
        let moved: Vec<(usize, f64)> =
            w.iter().filter_map(|&(a, v)| axes.iter().position(|&x| x == a).map(|i| (i, v))).collect();
        if g(53.0) || g(28.0) || g(30.0) || g(10.0) || g(92.0) {
            // Machine-coordinate moves and offset changes: the program
            // position afterwards is not known from the text.
            if g(28.0) || g(30.0) || g(92.0) {
                p.pos = [None; 3];
            }
            for (i, _) in &moved {
                p.pos[*i] = None;
            }
        } else if !moved.is_empty() && !cycle {
            for &(i, v) in &moved {
                p.pos[i] = Some(v);
            }
            if p.motion == Some(0) && moved.len() == 1 && moved[0].0 == 2 {
                p.rapid_z = Some(moved[0].1);
            }
        }
    }
    p.modal = modal.into_iter().flatten().collect();
    let at = lines.get(index).map(AsRef::as_ref);
    if let Some(at) = at {
        if let Some(c) = comment_only(at) {
            p.op = Some(c);
        }
        let w = words(at);
        p.changes_tool = w.iter().any(|&(a, b)| a == 'M' && (b == 6.0 || b == 61.0));
    }
    let arc_without_g = at.is_some_and(|at| {
        let w = words(at);
        matches!(p.motion, Some(2 | 3))
            && w.iter().any(|(a, _)| "XYZIJKR".contains(*a))
            && !w.iter().any(|&(a, v)| a == 'G' && v <= 3.0)
    });
    p.problem = if at.is_none() {
        Some("Pick a line of the program".into())
    } else if code_before || at.is_some_and(not_plain) {
        Some("This program uses o-words or parameters before here; run it from the start".into())
    } else if comp {
        Some("Cutter compensation is on at this line; pick a line before the G41/G42".into())
    } else if incremental {
        Some("Incremental moves (G91) are on at this line; pick another line".into())
    } else if cycle {
        Some("A drilling cycle is on at this line; pick the G8x line or one before it".into())
    } else if arc_without_g {
        Some("This line continues an arc; pick the line that starts it".into())
    } else if index < p.preamble().len() + usize::from(p.percent) {
        Some("Too near the top of the program; run it from the start".into())
    } else {
        None
    };
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
        let how = match (self.approach().is_empty(), self.changes_tool) {
            (_, true) | (true, _) => "Z goes to the top first",
            (false, false) => "Z goes to the top, then over the start and down",
        };
        format!("Line {}: {}. {how}.", self.index + 1, parts.join(", "))
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
        if self.spindle != 0 && self.rpm <= 0.0 && !self.changes_tool {
            return Some("No spindle speed (S) before this line".into());
        }
        None
    }

    /// Moves from machine top to where the start line begins: over the
    /// spot, rapid down to the retract plane, feed down to the height.
    fn approach(&self) -> Vec<String> {
        let [Some(x), Some(y), Some(z)] = self.pos else { return Vec::new() };
        if self.changes_tool {
            return Vec::new();
        }
        let mut v = vec![format!("G0 X{} Y{}", fmt(x), fmt(y))];
        let rapid = self.rapid_z.unwrap_or(z).max(z);
        v.push(format!("G0 Z{}", fmt(rapid)));
        if rapid > z {
            match self.feed {
                Some(f) if f > 0.0 => v.push(format!("G1 Z{} F{}", fmt(z), fmt(f))),
                _ => v.push(format!("G1 Z{}", fmt(z))),
            }
        }
        v
    }

    /// The lines that stand in for everything before the start line, kept
    /// short so lines near the top can still be start lines. Within a line
    /// LinuxCNC does F, S, M3/M4, M7/M8, then the G4 dwell.
    pub fn preamble(&self) -> Vec<String> {
        let mut v = vec!["G53 G0 Z0 (millgui restart: Z to the top)".to_string()];
        let mut modal = self.modal.clone();
        modal.extend(["G90".into(), "G40".into(), "G80".into()]);
        if !self.changes_tool {
            modal.extend(self.tlo.clone());
        }
        v.push(modal.join(" "));
        let mut setup = Vec::new();
        if let Some(f) = self.feed {
            setup.push(format!("F{}", fmt(f)));
        }
        let spin = self.spindle != 0 && !self.changes_tool;
        if spin {
            setup.push(format!("{} S{}", if self.spindle > 0 { "M3" } else { "M4" }, fmt(self.rpm)));
        }
        // M7 and M8 share a modal group: one per line.
        if self.mist {
            setup.push("M7".into());
        }
        if spin {
            setup.push("G4 P2".into());
        }
        if !setup.is_empty() {
            v.push(setup.join(" "));
        }
        if self.flood {
            v.push("M8".into());
        }
        let approach = self.approach();
        let last = approach.last().map(|l| if l.starts_with("G1") { 1 } else { 0 });
        v.extend(approach);
        // Leave the program's motion mode in force for lines that move
        // without a G word. (Arcs are refused unless the line has its G.)
        if let Some(m @ (0 | 1)) = self.motion {
            if last != Some(m) {
                v.push(format!("G{m}"));
            }
        }
        v
    }

    /// The restart copy: preamble, blank lines, then the program from the
    /// start line on, each at its original line number.
    pub fn file_lines<S: AsRef<str>>(&self, lines: &[S]) -> Vec<String> {
        let mut out = Vec::with_capacity(lines.len());
        if self.percent {
            out.push("%".to_string());
        }
        out.extend(self.preamble());
        while out.len() < self.index {
            out.push(String::new());
        }
        out.extend(lines.iter().skip(self.index).map(|l| l.as_ref().to_string()));
        out
    }
}

/// Where the restart copy of `program` goes.
pub fn copy_path(program: &str) -> PathBuf {
    let name = Path::new(program).file_name().map(|n| n.to_os_string()).unwrap_or_else(|| "program.ngc".into());
    std::env::temp_dir().join("millgui-restart").join(name)
}

/// A restart under way: runs the copy, then loads the original again once
/// the run ends (or never starts).
pub struct Restart {
    pub copy: String,
    pub original: String,
    started: Option<Instant>,
    seen_running: bool,
}

impl Restart {
    /// Writes the copy. Err is the reason to show.
    pub fn new<S: AsRef<str>>(plan: &Plan, lines: &[S], original: &str) -> Result<Self, String> {
        let path = copy_path(original);
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/tmp")))?;
            let mut text = plan.file_lines(lines).join("\n");
            text.push('\n');
            std::fs::write(&path, text)
        };
        write().map_err(|e| format!("Could not write the restart copy: {e}"))?;
        Ok(Self { copy: path.to_string_lossy().into_owned(), original: original.to_string(), started: None, seen_running: false })
    }

    /// Advance on each status (or clock tick). Returns false once finished.
    pub fn pump(&mut self, s: &Status, m: &Handle) -> bool {
        let Some(at) = self.started else {
            m.send(Command::ProgramOpen(self.copy.clone()));
            m.send(Command::ProgramRun(0));
            self.started = Some(Instant::now());
            return true;
        };
        if s.interp != Interp::Idle {
            self.seen_running = true;
            return true;
        }
        if !self.seen_running && at.elapsed() < Duration::from_secs(3) {
            return true;
        }
        m.send(Command::ProgramOpen(self.original.clone()));
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
        assert!(!p.changes_tool && p.problem.is_none(), "{:?}", p.problem);
        assert_eq!(
            p.preamble(),
            vec![
                "G53 G0 Z0 (millgui restart: Z to the top)",
                "G21 G17 G94 G91.1 G54 G90 G40 G80 G43 H3",
                "F300 M3 S8000 M7 G4 P2",
                "G0 X10 Y10",
                "G0 Z15",
                "G1 Z-1 F300",
            ]
        );
        // At the T5 M6 line: the line changes the tool, so no tool length,
        // spindle or approach; the program does those after the change.
        let p = plan(FUSION, 18);
        assert!(p.changes_tool);
        assert_eq!((p.tool, p.spindle, p.mist), (Some(3), 0, false));
        assert_eq!(p.preamble(), vec!["G53 G0 Z0 (millgui restart: Z to the top)", "G21 G17 G94 G91.1 G54 G90 G40 G80", "F300", "G0"]);
        let p = plan(FUSION, 20);
        assert_eq!((p.tool, p.rpm, p.spindle), (Some(5), 2000.0, 1));
        assert_eq!(p.op.as_deref(), Some("DRILL1"));
    }

    #[test]
    fn restart_copy_keeps_line_numbers() {
        let p = plan(FUSION, 13);
        let f = p.file_lines(FUSION);
        assert_eq!(f.len(), FUSION.len());
        assert_eq!(f[0], "%");
        assert_eq!(f[1], "G53 G0 Z0 (millgui restart: Z to the top)");
        assert_eq!(f[12], "");
        assert_eq!(&f[13..], &FUSION[13..]);
        // Before the preamble fits: refused.
        assert!(plan(FUSION, 2).problem.is_some());
    }

    #[test]
    fn approach_uses_retract_plane() {
        let g = ["%", "G21 G90", "G54", "T1 M6", "S3000 M3", "G0 X0 Y0", "G43 Z15 H1", "G0 Z5", "G1 Z-0.5 F600", "X50", "Y10", "X0", "G0 Z15", "%"];
        assert!(plan(&g, 2).problem.unwrap().contains("Too near"));
        let p = plan(&g, 10);
        assert!(p.problem.is_none(), "{:?}", p.problem);
        let pre = p.preamble();
        let at = pre.iter().position(|l| l == "G0 X50 Y0").expect("over the start");
        assert_eq!(&pre[at + 1..], &["G0 Z5", "G1 Z-0.5 F600"]);
        assert!(p.summary(1).ends_with("Z goes to the top, then over the start and down."));
    }

    #[test]
    fn plan_refuses_code_comp_cycles_and_arcs() {
        let g = ["o100 sub", "G0 X1", "o100 endsub", "G1 X2", "G1 X2", "G1 X2", "G1 X2", "G1 X2", "G1 X2", "G1 X2", "G1 X2", "G1 X2", "G1 X2", "G1 X2"];
        assert!(plan(&g, 13).problem.is_some(), "o-word code before");
        let mut g: Vec<String> = (0..14).map(|_| "G0 X0 Y0 Z5".to_string()).collect();
        assert!(plan(&g, 13).problem.is_none());
        g[11] = "G41 D3".into();
        assert!(plan(&g, 13).problem.unwrap().contains("Cutter"));
        g[11] = "G81 X1 Y1 Z-3 R2".into();
        assert!(plan(&g, 13).problem.unwrap().contains("drilling"));
        g[12] = "G80".into();
        assert!(plan(&g, 13).problem.is_none());
        g[11] = "G2 X1 Y1 I1 J0".into();
        g[12] = "G0 X0".into();
        g[13] = "X2 Y2 I1 J0".into();
        assert!(plan(&g, 13).problem.is_none(), "motion mode is G0 again");
        g[12] = "X3 Y3 I1 J0".into();
        assert!(plan(&g, 13).problem.unwrap().contains("arc"));
        g[12] = "G91".into();
        assert!(plan(&g, 13).problem.unwrap().contains("Incremental"));
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
        assert_eq!(p.summary(3), "Line 14: 2D CONTOUR1, T3, spindle CW 8000, mist. Z goes to the top, then over the start and down.");
        assert!(p.summary(1).contains("needs T3 (T1 loaded)"));
    }
}
