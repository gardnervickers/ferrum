//! OFFSETS screen (milestone 4): the nine work coordinate systems, G92.
//!
//! Every change is G-code sent through MDI (G10 L2/L20, G5x, G92.1), so
//! LinuxCNC's interpreter stays the only writer of the offsets.
//!
//! Sources: the active system comes from status (always current). The others
//! come from [RS274NGC]PARAMETER_FILE at startup and from the values this
//! screen writes. LinuxCNC saves that file when it shuts down, so a program
//! that changes an inactive system shows here only once it is made active.

use crate::config::Config;
use crate::history::{self, Target};
use crate::manual::KeypadTarget;
use crate::{Actions, Live, MainWindow, OffsetRow};
use linuxcnc::{Command, Handle, Status};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

pub const CODES: [&str; 9] = ["G54", "G55", "G56", "G57", "G58", "G59", "G59.1", "G59.2", "G59.3"];
/// Columns: X Y Z A, then XY rotation.
const COLS: [&str; 5] = ["X", "Y", "Z", "A", "R"];

/// Offsets from a LinuxCNC parameter file (`<number> <value>` lines).
/// G54 X is #5221; each system is 20 parameters on; R is the 10th.
pub fn parse_var(text: &str) -> [[f64; 5]; 9] {
    let mut out = [[0.0; 5]; 9];
    for line in text.lines() {
        let mut w = line.split_whitespace();
        let (Some(n), Some(v)) = (w.next().and_then(|n| n.parse::<i32>().ok()), w.next().and_then(|v| v.parse::<f64>().ok())) else { continue };
        if !(5221..5400).contains(&n) {
            continue;
        }
        let (sys, k) = ((n - 5221) / 20, (n - 5221) % 20);
        let col = match k {
            0..=3 => k as usize,
            9 => 4,
            _ => continue,
        };
        out[sys as usize][col] = v;
    }
    out
}

/// An edit to the active system that LinuxCNC has not confirmed yet.
struct Pending {
    old: [f64; 5],
    cause: String,
    sent: std::time::Instant,
}

/// How long status may still show the old values after an edit is sent.
const CONFIRM: std::time::Duration = std::time::Duration::from_secs(3);
/// "Zero here" predicts the result from the feedback position: close enough.
const PREDICTED: f64 = 0.01;

pub struct Offsets {
    /// Edits to the active system waiting for status to show them. They
    /// are logged then, with the values LinuxCNC really took.
    pending: [Option<Pending>; 9],
    table: [[f64; 5]; 9],
    selected: usize,
    status: Option<Status>,
    machine: Handle,
    ui: slint::Weak<MainWindow>,
}

impl Offsets {
    fn active(&self) -> Option<usize> {
        self.status.as_ref().map(|s| (s.g5x_index - 1).clamp(0, 8) as usize)
    }

    /// MDI is only allowed with the machine on and nothing running.
    fn ready(&self) -> Result<&Status, String> {
        match &self.status {
            Some(s) if !s.power_on() => Err("Turn the machine on first".into()),
            Some(s) if s.busy() => Err("Offsets can't change while a program or MDI command runs".into()),
            Some(s) => Ok(s),
            None => Err("Not connected to LinuxCNC".into()),
        }
    }

    fn mdi(&mut self, line: String) -> Result<(), String> {
        self.ready()?;
        self.machine.send(Command::Mdi(line));
        Ok(())
    }

    /// G10 L2: set values in a system directly. `cause` goes in the timeline.
    fn set(&mut self, sys: usize, vals: &[(usize, f64)], cause: &str) -> Result<(), String> {
        let words: Vec<String> = vals.iter().map(|(c, v)| format!("{}{v:.6}", COLS[*c])).collect();
        self.mdi(format!("G10 L2 P{} {}", sys + 1, words.join(" ")))?;
        let old = self.table[sys];
        for &(c, v) in vals {
            self.table[sys][c] = v;
        }
        self.sent(sys, old, cause);
        Ok(())
    }

    /// An edit went to LinuxCNC. Status confirms the active system, so its
    /// entry waits for that; another system cannot be confirmed and is
    /// logged as sent.
    fn sent(&mut self, sys: usize, old: [f64; 5], cause: &str) {
        if self.active() == Some(sys) {
            // A second edit before the first is confirmed keeps the first `old`.
            let old = self.pending[sys].take().map_or(old, |p| p.old);
            self.pending[sys] = Some(Pending { old, cause: cause.into(), sent: std::time::Instant::now() });
        } else {
            history::record(Target::Wcs(sys), &old, &self.table[sys], cause, [0.0; 2]);
        }
    }

    /// G10 L20: make the current position zero on one axis of a system.
    fn zero_here(&mut self, sys: usize, axis: usize) -> Result<(), String> {
        let s = self.ready()?;
        if s.rotation_xy != 0.0 && axis < 2 {
            // The expected value below ignores rotation; status corrects the active row.
            if self.active() != Some(sys) {
                return Err("Zero X/Y here needs the rotated system to be active".into());
            }
        }
        let v = s.actual[axis] - s.tool_offset[axis] - s.g92_offset[axis];
        self.mdi(format!("G10 L20 P{} {}0", sys + 1, COLS[axis]))?;
        let old = self.table[sys];
        self.table[sys][axis] = v;
        self.sent(sys, old, &format!("Zero {} here", COLS[axis]));
        Ok(())
    }

    /// DRO ZERO: one tap zeroes an axis of the active system. No hold, so
    /// the status bar says what it read and where to put it back.
    fn zero_active(&mut self, letter: &str) -> Result<(), String> {
        let axis = COLS[..4].iter().position(|c| c.eq_ignore_ascii_case(letter)).ok_or_else(|| format!("No axis {letter}"))?;
        let sys = self.active().ok_or("Not connected to LinuxCNC")?;
        let was = self.ready()?.work_position()[axis];
        self.zero_here(sys, axis)?;
        if let Some(ui) = self.ui.upgrade() {
            let live = ui.global::<Live>();
            let unit = if axis == 3 { "°" } else { "" };
            live.set_message(format!("{} {} zeroed (read {was:.3}{unit}). Undo on OFFSETS › TIMELINE", CODES[sys], COLS[axis]).into());
            live.set_message_error(false);
        }
        Ok(())
    }

    fn show(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        let active = self.active();
        let fmt = |v: f64| format!("{v:.3}");
        let rows: Vec<OffsetRow> = (0..9)
            .map(|i| {
                let r = self.table[i];
                OffsetRow {
                    wcs: CODES[i].into(),
                    x: fmt(r[0]).into(),
                    y: fmt(r[1]).into(),
                    z: fmt(r[2]).into(),
                    a: fmt(r[3]).into(),
                    rot: fmt(r[4]).into(),
                    active: active == Some(i),
                    used: r.iter().any(|v| *v != 0.0),
                }
            })
            .collect();
        live.set_offsets(ModelRc::new(VecModel::from(rows)));
        live.set_offsets_selected(self.selected as i32);
        live.set_offsets_sel_label(CODES[self.selected].into());
        let g92 = self.status.as_ref().map(|s| s.g92_offset).unwrap_or_default();
        let on = g92.iter().any(|v| *v != 0.0);
        live.set_offsets_g92(if on { format!("G92/G52 X{:.3} Y{:.3} Z{:.3} A{:.3}", g92[0], g92[1], g92[2], g92[3]) } else { String::new() }.into());
    }

    fn refresh(&self) {
        if let Some(ui) = self.ui.upgrade() {
            self.show(&ui);
        }
    }

    fn note(&self, r: Result<(), String>) {
        if let (Err(e), Some(ui)) = (r, self.ui.upgrade()) {
            let live = ui.global::<Live>();
            live.set_message(e.into());
            live.set_message_error(true);
        }
    }
}

/// Keypad values "off-x", "off-y", "off-z", "off-a", "off-r" (selected row).
struct Keys(Rc<RefCell<Offsets>>);

fn col(what: &str) -> Option<usize> {
    let c = what.strip_prefix("off-")?;
    COLS.iter().position(|x| x.eq_ignore_ascii_case(c))
}

impl KeypadTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        let c = col(what)?;
        let o = self.0.borrow();
        Some(format!("{} {}", CODES[o.selected], if c == 4 { "ROTATION" } else { COLS[c] }))
    }
    fn current(&self, what: &str) -> Option<f64> {
        let c = col(what)?;
        let o = self.0.borrow();
        Some(o.table[o.selected][c])
    }
    fn apply(&self, what: &str, v: f64) -> Option<Result<(), String>> {
        let c = col(what)?;
        let mut o = self.0.borrow_mut();
        let sel = o.selected;
        let r = o.set(sel, &[(c, v)], "Edited on OFFSETS");
        o.refresh();
        Some(r)
    }
}

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, sim: bool) -> Rc<RefCell<Offsets>> {
    // The simulator starts from its own offsets, not the machine's file.
    let table = if sim {
        [[0.0; 5]; 9]
    } else {
        let path = cfg.dir().join(cfg.get("RS274NGC", "PARAMETER_FILE").unwrap_or("linuxcnc.var"));
        std::fs::read_to_string(path).map(|t| parse_var(&t)).unwrap_or([[0.0; 5]; 9])
    };
    let o = Rc::new(RefCell::new(Offsets { pending: Default::default(), table, selected: 0, status: None, machine: machine.clone(), ui: ui.as_weak() }));
    crate::manual::add_keypad_target(Rc::new(Keys(o.clone())));
    let actions = ui.global::<Actions>();
    let t = o.clone();
    actions.on_offsets_select(move |i| {
        let mut o = t.borrow_mut();
        o.selected = (i.max(0) as usize).min(8);
        o.refresh();
    });
    let t = o.clone();
    actions.on_offsets_activate(move || {
        let mut o = t.borrow_mut();
        let line = CODES[o.selected].to_string();
        let r = o.mdi(line);
        o.note(r);
    });
    let t = o.clone();
    actions.on_offsets_zero(move |axis| {
        let mut o = t.borrow_mut();
        let sel = o.selected;
        let r = o.zero_here(sel, axis.clamp(0, 3) as usize);
        o.note(r);
        o.refresh();
    });
    let t = o.clone();
    actions.on_dro_zero(move |letter| {
        let mut o = t.borrow_mut();
        let r = o.zero_active(&letter);
        o.note(r);
        o.refresh();
    });
    let t = o.clone();
    actions.on_offsets_clear(move || {
        let mut o = t.borrow_mut();
        let sel = o.selected;
        let r = o.set(sel, &[(0, 0.0), (1, 0.0), (2, 0.0), (3, 0.0), (4, 0.0)], "Cleared on OFFSETS");
        o.note(r);
        o.refresh();
    });
    let t = o.clone();
    actions.on_offsets_clear_all(move || {
        let mut o = t.borrow_mut();
        let r = (0..9).try_for_each(|i| o.set(i, &[(0, 0.0), (1, 0.0), (2, 0.0), (3, 0.0), (4, 0.0)], "Clear all on OFFSETS"));
        o.note(r);
        o.refresh();
    });
    let t = o.clone();
    actions.on_offsets_clear_g92(move || {
        let mut o = t.borrow_mut();
        let r = o.mdi("G92.1".into());
        o.note(r);
    });
    o
}

/// Timeline restore: put earlier values back in a system (one G10 L2).
pub fn restore(o: &Rc<RefCell<Offsets>>, sys: usize, vals: &[(usize, f64)], cause: &str) -> Result<(), String> {
    if sys >= 9 || vals.is_empty() || vals.iter().any(|(c, _)| *c >= 5) {
        return Err("That entry is not a work offset".into());
    }
    let mut o = o.borrow_mut();
    let r = o.set(sys, vals, cause);
    o.refresh();
    r
}

fn differs_by(a: &[f64; 5], b: &[f64; 5], tol: f64) -> bool {
    a.iter().zip(b).any(|(x, y)| (x - y).abs() > tol)
}

/// Who changed the active system, as far as status can tell.
fn observed_cause(s: &Status, switched: bool) -> String {
    if switched {
        // The system was changed while another was active (a program or a
        // probe routine); this is the first time its real values are seen.
        return "Found when it became active (changed earlier)".into();
    }
    // Status says what mode the machine was in, not what wrote the offset.
    match s.mode {
        linuxcnc::Mode::Auto => {
            let name = std::path::Path::new(&s.file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.is_empty() { "During a program".into() } else { format!("During program {name}") }
        }
        linuxcnc::Mode::Mdi => "During MDI or a probe routine".into(),
        _ => "Outside millgui".into(),
    }
}

/// The XY rotation of a work system, as far as this screen knows it.
pub fn rotation(o: &Rc<RefCell<Offsets>>, sys: usize) -> f64 {
    o.borrow().table.get(sys).map_or(0.0, |r| r[4])
}

/// The travel check's view of the offsets: every work system as this
/// screen knows it, G92 and the active system from status.
pub fn preflight_live(o: &Rc<RefCell<Offsets>>, s: &Status) -> preview::preflight::Live {
    let t = o.borrow().table;
    let mut live = preview::preflight::Live { active: s.g5x_index.clamp(1, 9) as u8, ..Default::default() };
    for (i, row) in t.iter().enumerate() {
        live.wcs[i] = [row[0], row[1], row[2]];
        live.rotation[i] = row[4];
    }
    live.g92 = [s.g92_offset[0], s.g92_offset[1], s.g92_offset[2]];
    live
}

/// Status: the active system's offsets are always current here.
pub fn apply_status(ui: &MainWindow, o: &Rc<RefCell<Offsets>>, s: &Status) {
    let mut st = o.borrow_mut();
    let first = st.status.is_none();
    let a = (s.g5x_index - 1).clamp(0, 8) as usize;
    let mut row = [s.g5x_offset[0], s.g5x_offset[1], s.g5x_offset[2], s.g5x_offset[3], s.rotation_xy];
    for v in row.iter_mut() {
        if v.abs() < 1e-9 {
            *v = 0.0;
        }
    }
    let changed = first || st.table[a] != row || st.status.as_ref().is_some_and(|o| o.g5x_index != s.g5x_index || o.g92_offset != s.g92_offset);
    if !first {
        let switched = st.status.as_ref().is_some_and(|o| o.g5x_index != s.g5x_index);
        match st.pending[a].take() {
            // Our own edit, now in effect: log what LinuxCNC really holds.
            Some(p) if !differs_by(&row, &st.table[a], PREDICTED) => {
                history::record(Target::Wcs(a), &p.old, &row, &p.cause, [0.0; 2]);
            }
            // Sent but not executed yet: status still has the old values.
            // Keep showing the edit and wait.
            Some(p) if !history::differs(&row, &p.old) && p.sent.elapsed() < CONFIRM => {
                st.pending[a] = Some(p);
                st.status = Some(s.clone());
                return;
            }
            // Refused, or something else changed it: log what is seen.
            Some(p) => history::record(Target::Wcs(a), &p.old, &row, &observed_cause(s, switched), [0.0; 2]),
            None => history::record(Target::Wcs(a), &st.table[a], &row, &observed_cause(s, switched), [0.0; 2]),
        }
    }
    st.table[a] = row;
    if first {
        st.selected = a;
    }
    st.status = Some(s.clone());
    if changed {
        st.show(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn var_file() {
        let t = parse_var("5220\t1.000000\n5221\t215.637879\n5222\t-94.431314\n5223\t-322.443830\n5230\t15.0\n5241\t1.5\n5383\t-7.0\n");
        assert_eq!(t[0], [215.637879, -94.431314, -322.443830, 0.0, 15.0]);
        assert_eq!(t[1][0], 1.5);
        assert_eq!(t[8][2], -7.0);
    }
}
