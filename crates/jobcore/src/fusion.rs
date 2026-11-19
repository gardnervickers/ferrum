//! Split a Fusion 360 program into CAM ops for a job.
//!
//! The mill's post (`linuxcnc-probing.cps`) writes each operation's name as
//! a comment line, e.g. `(2D ADAPTIVE1)`, then the tool call, spindle, work
//! offset, coolant and the move to the start. This reads that shape and
//! nothing looser: anything it cannot account for refuses the whole split
//! with the reason, and the program can still be run whole from FILES. It
//! never guesses where an op starts or what an op's tool, speed, units or
//! work offset is.
//!
//! What it checks beyond the job compiler's own body rules:
//! - no o-words, parameters or expressions anywhere (probing ops, macros);
//! - before the first op, only modal setup lines (`G90 G94 G17 G91.1`,
//!   `G21`, `G53 G0 Z0`...), and exactly one of G20/G21;
//! - every op knows its tool (a `Tn M6` in it or an earlier op), spindle
//!   speed and direction, work offset and coolant before it first moves;
//! - every op's first move is a rapid and it moves Z by rapid before its
//!   first cut, so an op name that is really a comment in the middle of a
//!   toolpath is caught rather than split there.

use crate::gcode::{oword, strip_comment, words};
use crate::model::Setup;

#[derive(Debug, Clone, PartialEq)]
pub struct CamOp {
    pub name: String,
    pub setup: Setup,
    pub body: Vec<String>,
}

/// The comment of a comment-only line.
fn comment_only(line: &str) -> Option<String> {
    let t = line.trim();
    let inner = t.strip_prefix('(')?.strip_suffix(')')?;
    (!inner.contains(['(', ')'])).then(|| inner.trim().to_string())
}

fn is_blank(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t == "%"
}

type Words = Vec<(char, f64)>;

fn plain_words(n: usize, line: &str) -> Result<Words, String> {
    let code = strip_comment(line);
    if oword(line).is_some() || code.contains(['#', '[', ']', '<', '>']) {
        return Err(format!(
            "line {n} ({}) uses o-words or parameters, as probing ops and macros do. The job cannot check those",
            line.trim()
        ));
    }
    let mut out = Vec::new();
    for (l, v) in words(line) {
        match v {
            Some(v) => out.push((l, v)),
            None => return Err(format!("line {n} ({}) has a computed value", line.trim())),
        }
    }
    Ok(out)
}

fn has(w: &Words, l: char, v: f64) -> bool {
    w.iter().any(|&(a, b)| a == l && (b - v).abs() < 1e-6)
}

fn get(w: &Words, l: char) -> Option<f64> {
    w.iter().find(|(a, _)| *a == l).map(|&(_, v)| v)
}

/// Modal state carried from line to line and op to op.
#[derive(Default, Clone)]
struct State {
    units: Option<&'static str>,
    wcs: Option<String>,
    tool: Option<i64>,
    rpm: Option<f64>,
    /// "CW", "CCW" or "OFF".
    spindle: Option<&'static str>,
    mist: bool,
    flood: bool,
    feed: Option<f64>,
    motion: Option<u8>,
}

impl State {
    fn apply(&mut self, w: &Words) {
        for &(l, v) in w {
            match l {
                'S' => self.rpm = Some(v),
                'F' => self.feed = Some(v),
                'M' if v == 3.0 => self.spindle = Some("CW"),
                'M' if v == 4.0 => self.spindle = Some("CCW"),
                'M' if v == 5.0 => self.spindle = Some("OFF"),
                'M' if v == 7.0 => self.mist = true,
                'M' if v == 8.0 => self.flood = true,
                'M' if v == 9.0 => {
                    self.mist = false;
                    self.flood = false;
                }
                'G' if v == 20.0 => self.units = Some("IN"),
                'G' if v == 21.0 => self.units = Some("MM"),
                'G' if [0.0, 1.0, 2.0, 3.0].contains(&v) => self.motion = Some(v as u8),
                'G' => {
                    if let Some(c) = crate::gcode::wcs_code(v) {
                        self.wcs = Some(c.to_string());
                    }
                }
                _ => {}
            }
        }
    }
}

/// A line that moves in program coordinates (not a G53 move).
fn moves(w: &Words) -> bool {
    w.iter().any(|(a, _)| "XYZABCIJKR".contains(*a)) && !has(w, 'G', 53.0) && !has(w, 'G', 28.0) && !has(w, 'G', 30.0)
}

/// Header lines: only these codes before the first op.
fn header_ok(w: &Words) -> bool {
    let g53_top = has(w, 'G', 53.0) && has(w, 'G', 0.0) && get(w, 'Z') == Some(0.0)
        && w.iter().all(|(a, _)| *a == 'G' || *a == 'Z');
    g53_top
        || w.iter().all(|&(a, v)| match a {
            'G' => [90.0, 94.0, 17.0, 91.1, 20.0, 21.0, 40.0, 49.0, 80.0, 64.0, 97.0, 98.0, 54.0].contains(&v),
            'P' => has(w, 'G', 64.0),
            'M' => v == 5.0 || v == 9.0,
            _ => false,
        })
}

/// Lines a program ends with, dropped from the last op's tail.
fn tail_line(w: &Words) -> bool {
    w.is_empty()
        || w.iter().all(|&(a, v)| a == 'M' && [2.0, 5.0, 9.0, 30.0].contains(&v))
        || (has(w, 'G', 53.0) && !w.iter().any(|(a, _)| "XY".contains(*a)))
}

pub fn split(text: &str) -> Result<Vec<CamOp>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let parsed: Vec<Words> = lines.iter().enumerate().map(|(i, l)| plain_words(i + 1, l)).collect::<Result<_, _>>()?;
    let first_code = lines
        .iter()
        .position(|l| !is_blank(l) && comment_only(l).is_none())
        .ok_or("the file has no G-code")?;
    let markers: Vec<usize> = (first_code..lines.len()).filter(|&i| comment_only(lines[i]).is_some()).collect();
    let Some(&first_op) = markers.first() else {
        return Err("no op names found. Fusion writes each op's name as a comment line, like (2D ADAPTIVE1), \
                    and this program has none, so it is not split"
            .into());
    };
    let mut st = State::default();
    for i in first_code..first_op {
        if is_blank(lines[i]) {
            continue;
        }
        let w = &parsed[i];
        if has(w, 'G', 91.0) || has(w, 'G', 90.1) {
            return Err(format!("line {} ({}) sets incremental mode", i + 1, lines[i].trim()));
        }
        if !header_ok(w) {
            return Err(format!("line {} ({}) comes before the first op and is not a setup line", i + 1, lines[i].trim()));
        }
        if has(w, 'G', 20.0) && has(w, 'G', 21.0) {
            return Err(format!("line {} sets both inch and mm", i + 1));
        }
        st.apply(w);
    }
    if st.units.is_none() {
        return Err("there is no G21 (mm) or G20 (inch) before the first op, so the units are unknown".into());
    }
    let mut ops = Vec::new();
    for (k, &start) in markers.iter().enumerate() {
        let end = markers.get(k + 1).copied().unwrap_or(lines.len());
        let name = comment_only(lines[start]).unwrap_or_default();
        let label = if name.is_empty() { format!("the op on line {}", start + 1) } else { format!("op {name}") };
        if name.is_empty() {
            return Err(format!("{label} has no name"));
        }
        let mut body: Vec<String> = Vec::new();
        let mut setup: Option<State> = None;
        let mut z_set = false;
        for i in start + 1..end {
            let line = lines[i];
            if is_blank(line) {
                continue;
            }
            let w = &parsed[i];
            if has(w, 'G', 91.0) || has(w, 'G', 90.1) {
                return Err(format!("line {} ({}) in {label} sets incremental mode", i + 1, line.trim()));
            }
            if has(w, 'M', 6.0) {
                // The tool call: only T and M6 on the line, before any move.
                let t = get(w, 'T').ok_or_else(|| format!("line {} ({}) in {label} changes tool without a T", i + 1, line.trim()))?;
                if setup.is_some() || w.iter().any(|&(a, v)| !(a == 'T' || (a == 'M' && v == 6.0))) {
                    return Err(format!("line {} ({}) in {label}: a tool change only goes at the start of an op", i + 1, line.trim()));
                }
                st.tool = Some(t as i64);
                continue;
            }
            if w.len() == 1 && w[0].0 == 'T' && setup.is_none() {
                // Tool preselect: the job loads tools itself.
                continue;
            }
            if moves(w) && setup.is_none() {
                // First move of the op: a rapid, with everything known.
                let mut probe = st.clone();
                probe.apply(w);
                if probe.motion != Some(0) || has(w, 'G', 1.0) {
                    return Err(format!(
                        "{label}'s first move (line {}, {}) is a cut, not a rapid. The op name may be a comment \
                         in the middle of a toolpath",
                        i + 1,
                        line.trim()
                    ));
                }
                setup = Some(st.clone());
            }
            st.apply(w);
            if setup.is_none() && tail_line(w) {
                // M5, M9, G53 G0 Z0 ahead of the tool call: the job's own.
                continue;
            }
            if setup.is_some() && moves(w) {
                if get(w, 'Z').is_some() && st.motion == Some(0) {
                    z_set = true;
                }
                if st.motion.is_some_and(|m| m != 0) && !z_set {
                    return Err(format!(
                        "{label} cuts (line {}, {}) before it moves Z down by rapid",
                        i + 1,
                        line.trim()
                    ));
                }
            }
            body.push(line.to_string());
        }
        let Some(s) = setup else {
            return Err(format!("{label} has no moves"));
        };
        // Drop the program end and the retract before the next tool change.
        while body.last().is_some_and(|l| tail_line(&plain_words(0, l).unwrap_or_default()) && !moves(&plain_words(0, l).unwrap_or_default())) {
            body.pop();
        }
        let tool = s.tool.ok_or_else(|| format!("{label} has no tool: no Tn M6 in it or before it"))?;
        let rpm = s.rpm.filter(|r| *r > 0.0).ok_or_else(|| format!("{label} has no spindle speed (S) before its first move"))?;
        let dir = match s.spindle {
            Some(d @ ("CW" | "CCW")) => d,
            _ => return Err(format!("{label} starts moving with the spindle off (no M3 or M4 before its first move)")),
        };
        let wcs = s.wcs.clone().ok_or_else(|| format!("{label} has no work offset (G54...) before its first move"))?;
        let coolant = match (s.mist, s.flood) {
            (true, true) => return Err(format!("{label} has mist and flood on together")),
            (true, false) => "MIST",
            (false, true) => "FLOOD",
            (false, false) => "OFF",
        };
        let units = s.units.ok_or("units unknown")?;
        ops.push(CamOp { name, setup: Setup::typed(units, &wcs, tool, rpm, dir, coolant, s.feed), body });
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRACKET: &str = include_str!("../../../testdata/nc_files/bracket-sample.ngc");

    #[test]
    fn splits_the_bracket_sample() {
        let ops = split(BRACKET).unwrap();
        let names: Vec<_> = ops.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["FACE1", "2D CONTOUR1"]);
        assert_eq!(ops[0].setup, Setup::typed("MM", "G54", 14, 4000.0, "CW", "OFF", None));
        assert_eq!(ops[1].setup, Setup::typed("MM", "G54", 23, 8000.0, "CW", "OFF", Some(600.0)));
        assert_eq!(ops[0].body.first().map(String::as_str), Some("S4000 M3"));
        assert_eq!(ops[0].body.last().map(String::as_str), Some("G0 Z15."));
        // The retract and tool call are the job's: not in the body.
        assert!(!ops[1].body.iter().any(|l| l.contains("M6") || l.contains("G53")), "{:?}", ops[1].body);
        assert_eq!(ops[1].body.last().map(String::as_str), Some("G0 Z15."));
    }

    #[test]
    fn same_tool_ops_inherit_tool_speed_and_coolant() {
        let p = "%\n(T3 D=6.)\nG90 G94 G17 G91.1\nG21\n(ADAPTIVE1)\nT3 M6\nS8000 M3\nG54\nM8\nG0 X0 Y0\nG43 Z15. H3\nG1 Z-1. F300.\nX10.\nG0 Z15.\n(CONTOUR1)\nG0 X5. Y5.\nZ5.\nG1 Z-2.\nX20.\nG0 Z15.\nM9\nM5\nG53 G0 Z0.\nM30\n%\n";
        let ops = split(p).unwrap();
        assert_eq!(ops[1].setup, Setup::typed("MM", "G54", 3, 8000.0, "CW", "FLOOD", Some(300.0)));
        assert_eq!(ops[1].body, ["G0 X5. Y5.", "Z5.", "G1 Z-2.", "X20.", "G0 Z15."]);
    }

    fn refused(p: &str, why: &str) {
        match split(p) {
            Err(e) => assert!(e.contains(why), "{e}"),
            Ok(ops) => panic!("split into {} ops", ops.len()),
        }
    }

    #[test]
    fn refuses_what_it_cannot_account_for() {
        let ok = "G90 G21\n(A)\nT1 M6\nS1000 M3\nG54\nG0 X0 Y0\nG43 Z5 H1\nG1 Z-1 F100\n";
        assert!(split(ok).is_ok());
        refused("G90 G21\nG0 X0 Y0\nG1 Z-1 F100\n", "no op names");
        refused("G90\n(A)\nT1 M6\nS1000 M3\nG54\nG0 X0\nG0 Z5\n", "units are unknown");
        refused(&ok.replace("G0 X0 Y0", "o<probe> call [1]"), "o-words or parameters");
        refused(&ok.replace("G0 X0 Y0", "#1 = 5"), "o-words or parameters");
        refused(&ok.replace("S1000 M3\n", ""), "spindle");
        refused(&ok.replace("T1 M6\n", ""), "no tool");
        refused(&ok.replace("G54\n", ""), "work offset");
        refused(&ok.replace("G90 G21", "G90 G21\nG0 X5"), "not a setup line");
        refused(&ok.replace("G43 Z5 H1", "G43 Z5 H1\nG91"), "incremental");
        // A comment in the middle of a toolpath is not taken as an op.
        refused(&format!("{ok}(NOTE)\nX5\nX10\n"), "is a cut, not a rapid");
        refused(&ok.replace("G0 X0 Y0\nG43 Z5 H1", "G0 X0 Y0"), "before it moves Z down by rapid");
        refused(&ok.replace("(A)\nT1 M6", "(A)\n(NOTE)\nT1 M6"), "has no moves");
    }
}
