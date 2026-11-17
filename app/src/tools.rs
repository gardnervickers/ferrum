//! TOOLS screen (milestone 4): the tool table, a check of the loaded
//! program's tools, M6 / M61 Q0, and the drawbar outside a tool change.
//!
//! The table file ([EMCIO]TOOL_TABLE) is the source: LinuxCNC rewrites it
//! whenever it changes tool data (G10 L1/L10, touch off), so edits here
//! write the file and send LoadToolTable, as Probe Basic's table editor did.
//! Edits are refused while a program or MDI command runs.
//!
//! Wear: LinuxCNC's mill tool table has no wear columns, so the table value
//! stays the only number the machine uses. A wear adjustment adds a signed
//! amount to a tool's length or diameter. The running total is kept beside
//! the table value it was made to (history.rs) and shown next to it; it
//! ends when the table holds any other value (a touch off, a typed edit).

use crate::config::Config;
use crate::history::{self, Target};
use crate::manual::KeypadTarget;
use crate::toolchange::{Io, SCRATCH};
use crate::{Actions, CheckRow, Live, MainWindow, ToolRow};
use linuxcnc::{Command, Handle, Status};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// One tool table line. Words other than T, Z and D (pocket, other axis
/// offsets, orientation) are kept as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub t: i32,
    pub z: f64,
    pub d: f64,
    pub other: Vec<String>,
    pub comment: String,
}

pub fn parse(text: &str) -> Vec<Tool> {
    let mut out = Vec::new();
    for line in text.lines() {
        let (data, comment) = match line.split_once(';') {
            Some((d, c)) => (d, c.trim().to_string()),
            None => (line, String::new()),
        };
        let mut tool = Tool { t: -1, z: 0.0, d: 0.0, other: Vec::new(), comment };
        for w in data.split_whitespace() {
            let (l, v) = w.split_at(1);
            match l.to_ascii_uppercase().as_str() {
                "T" => tool.t = v.parse().unwrap_or(-1),
                "Z" => tool.z = v.parse().unwrap_or(0.0),
                "D" => tool.d = v.parse().unwrap_or(0.0),
                _ => tool.other.push(w.to_string()),
            }
        }
        if tool.t >= 0 {
            out.push(tool);
        }
    }
    out.sort_by_key(|t| t.t);
    out
}

pub fn format(tools: &[Tool]) -> String {
    let mut s = String::from(";Tool  Pocket Z Offset     Diameter     Remark\n");
    for t in tools {
        let mut other = t.other.clone();
        if !other.iter().any(|w| w.starts_with(['P', 'p'])) {
            other.insert(0, format!("P{}", t.t));
        }
        s.push_str(&format!("{:<6} {:<6} Z{:<+11.6} D{:<+11.6} ; {}\n", format!("T{}", t.t), other.join(" "), t.z, t.d, t.comment));
    }
    s
}

/// T words in a program (any `T<n>`, outside comments).
pub fn program_tools<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<i32> {
    let mut out: Vec<i32> = Vec::new();
    for line in lines {
        let mut code = String::new();
        let mut depth = 0;
        for c in line.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth = (depth - 1).max(0),
                ';' if depth == 0 => break,
                _ if depth == 0 => code.push(c.to_ascii_uppercase()),
                _ => {}
            }
        }
        let b = code.as_bytes();
        for (i, &c) in b.iter().enumerate() {
            if c == b'T' {
                let n: String = code[i + 1..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = n.parse::<i32>() {
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
            }
        }
    }
    out
}

pub struct Tools {
    path: PathBuf,
    tools: Vec<Tool>,
    selected: Option<i32>,
    status: Option<Status>,
    machine: Handle,
    io: Option<Rc<dyn Io>>,
    ui: slint::Weak<MainWindow>,
    probe_tool: i32,
}

impl Tools {
    fn load(&mut self) {
        // A failed or empty read (LinuxCNC may be rewriting the file)
        // keeps the table as it was; the next reload picks it up.
        let Some(new) = std::fs::read_to_string(&self.path).ok().map(|t| parse(&t)).filter(|t| !t.is_empty() || self.tools.is_empty()) else { return };
        let old = std::mem::replace(&mut self.tools, new);
        // A tool this screen knew now has other values: a touch off, G10
        // L1, or the file edited by hand.
        for o in &old {
            if let Some(n) = self.tools.iter().find(|n| n.t == o.t) {
                history::record(Target::Tool(o.t), &[o.z, o.d], &[n.z, n.d], "Tool table changed outside millgui", [0.0; 2]);
            }
        }
    }

    /// Write `new` (length, diameter) to a tool, logged with `cause`.
    /// `wear`: the adjustment then in effect on each value (`None` leaves
    /// that value's wear as it is).
    fn write(&mut self, t: i32, new: [f64; 2], wear: [Option<f64>; 2], cause: &str) -> Result<(), String> {
        self.idle()?;
        if new[1] < 0.0 {
            return Err("Diameter can't be negative".into());
        }
        let tool = self.tool_mut(t).ok_or(format!("T{t} is not in the tool table"))?;
        let old = [tool.z, tool.d];
        (tool.z, tool.d) = (new[0], new[1]);
        if let Err(e) = self.save() {
            // Not written: the table in memory must not say otherwise.
            if let Some(tool) = self.tool_mut(t) {
                (tool.z, tool.d) = (old[0], old[1]);
            }
            return Err(e);
        }
        for axis in 0..2 {
            if let Some(w) = wear[axis] {
                history::set_wear(t, axis, w, new[axis]);
            }
        }
        history::record(Target::Tool(t), &old, &new, cause, history::wear(t, new));
        // LinuxCNC applies a new length at the next G43, not at once.
        if (old[0] - new[0]).abs() > history::EPS && self.status.as_ref().is_some_and(|s| s.tool_in_spindle == t) {
            self.note(format!("T{t} is in the spindle: its new length applies at the next G43 or tool change"), false);
        }
        Ok(())
    }

    /// Change the selected tool's length (`axis` 0) or diameter (1): to
    /// `v`, or by `v` when it is a wear adjustment.
    fn edit(&mut self, axis: usize, v: f64, adjust: bool) -> Result<(), String> {
        let sel = self.selected.ok_or("Select a tool first")?;
        let t = self.tools.iter().find(|x| x.t == sel).ok_or("Select a tool first")?;
        let old = [t.z, t.d];
        let mut new = old;
        let mut wear = [None; 2];
        if adjust {
            new[axis] = old[axis] + v;
            wear[axis] = Some(history::wear(sel, old)[axis] + v);
        } else {
            // A typed value is a new measurement: its wear starts again.
            new[axis] = v;
            wear[axis] = Some(0.0);
        }
        let what = ["length", "diameter"][axis];
        let cause = if adjust { format!("Wear adjust {v:+.3} {what}") } else { "Edited on TOOLS".to_string() };
        self.write(sel, new, wear, &cause)
    }

    fn idle(&self) -> Result<(), String> {
        match &self.status {
            Some(s) if s.busy() => Err("The tool table can't change while a program or MDI command runs".into()),
            _ => Ok(()),
        }
    }

    /// Write the table and have LinuxCNC reload it.
    fn save(&mut self) -> Result<(), String> {
        self.idle()?;
        let tmp = self.path.with_extension("tbl.millgui");
        std::fs::write(&tmp, format(&self.tools)).map_err(|e| format!("tool table: {e}"))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("tool table: {e}"))?;
        self.machine.send(Command::LoadToolTable);
        Ok(())
    }

    fn tool_mut(&mut self, t: i32) -> Option<&mut Tool> {
        self.tools.iter_mut().find(|x| x.t == t)
    }

    fn add(&mut self, t: i32, comment: &str) -> Result<(), String> {
        if !(1..=99999).contains(&t) {
            return Err("Tool numbers start at 1".into());
        }
        if self.tools.iter().any(|x| x.t == t) {
            self.selected = Some(t);
            return Err(format!("T{t} is already in the table"));
        }
        self.idle()?;
        self.tools.push(Tool { t, z: 0.0, d: 0.0, other: Vec::new(), comment: comment.into() });
        self.tools.sort_by_key(|t| t.t);
        self.selected = Some(t);
        self.save()
    }

    /// The selected tool's comment, as it shows in the table and the tool
    /// change overlay. The table file keeps it after the ";".
    fn set_comment(&mut self, text: &str) -> Result<(), String> {
        let sel = self.selected.ok_or("Select a tool first")?;
        self.idle()?;
        let text = text.trim().to_string();
        let t = self.tool_mut(sel).ok_or("Select a tool first")?;
        if t.comment == text {
            return Ok(());
        }
        t.comment = text;
        self.save()
    }

    fn next_scratch(&self) -> Option<i32> {
        SCRATCH.into_iter().find(|n| !self.tools.iter().any(|t| t.t == *n))
    }

    fn show(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        let cur = self.status.as_ref().map_or(-1, |s| s.tool_in_spindle);
        let rows: Vec<ToolRow> = self
            .tools
            .iter()
            .map(|t| ToolRow {
                wear_len: wear_text(history::wear(t.t, [t.z, t.d])[0]).into(),
                wear_dia: wear_text(history::wear(t.t, [t.z, t.d])[1]).into(),
                t: format!("T{}", t.t).into(),
                len: if t.z == 0.0 { "".into() } else { format!("{:.3}", t.z).into() },
                dia: format!("{:.3}", t.d).into(),
                comment: t.comment.clone().into(),
                needs_len: t.z == 0.0 && t.t != self.probe_tool,
                current: t.t == cur,
                scratch: SCRATCH.contains(&t.t),
            })
            .collect();
        live.set_tools(ModelRc::new(VecModel::from(rows)));
        let sel = self.selected.and_then(|s| self.tools.iter().position(|t| t.t == s)).map_or(-1, |i| i as i32);
        live.set_tools_selected(sel);
        live.set_tools_sel_label(self.selected.map(|t| format!("T{t}")).unwrap_or_default().into());
        self.show_check(ui);
        self.show_drawbar(ui);
    }

    /// The loaded program's tools against the table.
    fn show_check(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        let lines: Vec<slint::SharedString> = live.get_gcode_lines().iter().collect();
        let used = program_tools(lines.iter().map(|s| s.as_str()));
        let check: Vec<CheckRow> = used
            .iter()
            .filter(|&&n| n > 0)
            .map(|&n| {
                let (status, kind, action) = match self.tools.iter().find(|t| t.t == n) {
                    None => ("MISSING", "bad", "Create the entry, then touch off"),
                    Some(t) if t.z == 0.0 && n != self.probe_tool => ("NEEDS LENGTH", "warn", "Touch off the tool length"),
                    Some(_) => ("READY", "good", ""),
                };
                CheckRow { t: format!("T{n}").into(), status: status.into(), kind: kind.into(), action: action.into() }
            })
            .collect();
        live.set_tools_missing(check.iter().any(|c| c.status == "MISSING"));
        live.set_tools_check(ModelRc::new(VecModel::from(check)));
    }

    /// Drawbar outside a tool change.
    fn show_drawbar(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        // Table comments for the tool change overlay.
        let desc = |t: &str| t.trim_start_matches('T').parse::<i32>().ok().and_then(|n| self.tools.iter().find(|x| x.t == n)).map(|x| x.comment.clone()).unwrap_or_default();
        live.set_tc_from_desc(desc(&live.get_tc_from()).into());
        live.set_tc_tool_desc(desc(&live.get_tc_tool()).into());
        let (state, can_extend, can_retract) = match &self.io {
            Some(io) => {
                let i = io.read();
                let idle = self.status.as_ref().is_some_and(|s| !s.busy() && s.power_on());
                let spindle_off = i.spindle_request == Some(false);
                let label = match i.drawbar_extended {
                    Some(true) => "EXTENDED",
                    Some(false) => "RETRACTED",
                    None => "UNKNOWN",
                };
                // Clamping with a spindle command pending would start the
                // spindle the moment the drawbar retracts (a safeguard: the
                // overlay's RESUME makes the same check).
                (label, idle && spindle_off && !i.change && i.drawbar_extended == Some(false), spindle_off && (i.manual_request || i.drawbar_extended == Some(true)))
            }
            None => ("NOT FITTED", false, false),
        };
        live.set_tools_drawbar(state.into());
        live.set_tools_can_extend(can_extend);
        live.set_tools_can_retract(can_retract);
    }

    fn note(&self, text: impl Into<String>, error: bool) {
        if let Some(ui) = self.ui.upgrade() {
            let live = ui.global::<Live>();
            live.set_message(text.into().into());
            live.set_message_error(error);
        }
    }

    fn refresh(&self) {
        if let Some(ui) = self.ui.upgrade() {
            self.show(&ui);
        }
    }
}

/// "-0.020": a wear adjustment beside its value, empty for none.
fn wear_text(w: f64) -> String {
    if w.abs() > history::EPS { format!("{w:+.3}") } else { String::new() }
}

/// Timeline restore: put a tool's earlier length (0) and/or diameter (1)
/// back, with the wear adjustment that was in effect on them then.
pub fn restore(tools: &Rc<RefCell<Tools>>, t: i32, vals: &[(usize, f64)], wear_then: [f64; 2], cause: &str) -> Result<(), String> {
    let mut st = tools.borrow_mut();
    let tool = st.tools.iter().find(|x| x.t == t).ok_or(format!("T{t} is no longer in the tool table"))?;
    let mut new = [tool.z, tool.d];
    let mut wear = [None; 2];
    for &(axis, v) in vals {
        if axis >= 2 {
            return Err("That entry is not a tool".into());
        }
        new[axis] = v;
        wear[axis] = Some(wear_then[axis]);
    }
    let r = st.write(t, new, wear, cause);
    st.refresh();
    r
}

/// Set a tool's diameter as a wear adjustment (a job check's correction):
/// the change is added to the diameter wear the timeline keeps.
pub fn adjust_diameter(tools: &Rc<RefCell<Tools>>, t: i32, d: f64, cause: &str) -> Result<(), String> {
    let mut st = tools.borrow_mut();
    let tool = st.tools.iter().find(|x| x.t == t).ok_or(format!("T{t} is not in the tool table"))?;
    let old = [tool.z, tool.d];
    let wear = [None, Some(history::wear(t, old)[1] + (d - old[1]))];
    let r = st.write(t, [old[0], d], wear, cause);
    st.refresh();
    r
}

/// Keypad values: "tool-z", "tool-d" (selected tool), their wear
/// adjustments "tool-adj-z", "tool-adj-d", and "tool-new".
struct Keys(Rc<RefCell<Tools>>);

impl KeypadTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        let t = self.0.borrow().selected.unwrap_or(0);
        Some(match what {
            "tool-z" => format!("T{t} LENGTH Z"),
            "tool-d" => format!("T{t} DIAMETER"),
            "tool-adj-z" => format!("T{t} ADD TO LENGTH (+ CUTS SHALLOWER)"),
            "tool-adj-d" => format!("T{t} ADD TO DIAMETER"),
            "tool-new" => "NEW TOOL NUMBER".into(),
            _ => return None,
        })
    }
    fn current(&self, what: &str) -> Option<f64> {
        let st = self.0.borrow();
        let tool = st.selected.and_then(|s| st.tools.iter().find(|t| t.t == s));
        match what {
            "tool-z" => Some(tool.map_or(0.0, |t| t.z)),
            "tool-d" => Some(tool.map_or(0.0, |t| t.d)),
            "tool-new" | "tool-adj-z" | "tool-adj-d" => Some(0.0),
            _ => None,
        }
    }
    fn apply(&self, what: &str, v: f64) -> Option<Result<(), String>> {
        let mut st = self.0.borrow_mut();
        let r = match what {
            "tool-new" => st.add(v as i32, ""),
            "tool-z" => st.edit(0, v, false),
            "tool-d" => st.edit(1, v, false),
            "tool-adj-z" => st.edit(0, v, true),
            "tool-adj-d" => st.edit(1, v, true),
            _ => return None,
        };
        st.refresh();
        Some(r)
    }
}

impl crate::textentry::TextTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        (what == "tool-comment").then(|| format!("T{} COMMENT", self.0.borrow().selected.unwrap_or(0)))
    }
    fn current(&self, what: &str) -> Option<String> {
        let st = self.0.borrow();
        (what == "tool-comment").then(|| st.selected.and_then(|s| st.tools.iter().find(|t| t.t == s)).map(|t| t.comment.clone()).unwrap_or_default())
    }
    fn apply(&self, what: &str, v: &str) -> Option<Result<(), String>> {
        if what != "tool-comment" {
            return None;
        }
        let mut st = self.0.borrow_mut();
        let r = st.set_comment(v);
        st.refresh();
        Some(r)
    }
}

pub fn table_path(cfg: &Config, sim: bool) -> PathBuf {
    let rel = cfg.get("EMCIO", "TOOL_TABLE").unwrap_or("tool.tbl");
    let real = cfg.dir().join(rel);
    if !sim {
        return real;
    }
    // The simulator edits a copy, never the machine's table.
    let dir = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(".cache/millgui");
    let copy = dir.join("sim-tool.tbl");
    if !copy.exists() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::copy(&real, &copy);
    }
    copy
}

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, io: Option<Rc<dyn Io>>, sim: bool) -> Rc<RefCell<Tools>> {
    wire_path(ui, machine, cfg, io, table_path(cfg, sim))
}

pub fn wire_path(ui: &MainWindow, machine: &Handle, cfg: &Config, io: Option<Rc<dyn Io>>, path: PathBuf) -> Rc<RefCell<Tools>> {
    let probe_tool = cfg.get("PROBE", "TOOL_NUMBER").and_then(|v| v.trim().parse().ok()).unwrap_or(99);
    let tools = Rc::new(RefCell::new(Tools { path, tools: Vec::new(), selected: None, status: None, machine: machine.clone(), io, ui: ui.as_weak(), probe_tool }));
    tools.borrow_mut().load();
    crate::manual::add_keypad_target(Rc::new(Keys(tools.clone())));
    crate::textentry::add_target(Rc::new(Keys(tools.clone())));
    let actions = ui.global::<Actions>();
    let t = tools.clone();
    actions.on_tools_select(move |i| {
        let mut st = t.borrow_mut();
        st.selected = st.tools.get(i as usize).map(|x| x.t);
        st.refresh();
    });
    let t = tools.clone();
    actions.on_tools_m6(move || {
        let st = t.borrow();
        match st.selected {
            Some(n) => st.machine.send(Command::Mdi(format!("M6 T{n} G43"))),
            None => st.note("Select a tool first", true),
        }
    });
    let t = tools.clone();
    actions.on_tools_unload(move || {
        let st = t.borrow();
        let cur = st.status.as_ref().map_or(0, |s| s.tool_in_spindle);
        st.machine.send(Command::Mdi("M61 Q0".into()));
        if cur > 0 {
            st.note(format!("LinuxCNC now has no tool loaded. Take T{cur} out of the spindle."), false);
        }
    });
    let t = tools.clone();
    actions.on_tools_add_scratch(move || {
        let mut st = t.borrow_mut();
        let r = match st.next_scratch() {
            Some(n) => st.add(n, "Scratch tool"),
            None => Err("All scratch tools T200–249 are in use".into()),
        };
        if let Err(e) = r {
            st.note(e, true);
        }
        st.refresh();
    });
    let t = tools.clone();
    actions.on_tools_delete(move || {
        let mut st = t.borrow_mut();
        let cur = st.status.as_ref().map_or(-1, |s| s.tool_in_spindle);
        let r = match st.selected {
            None => Err("Select a tool first".to_string()),
            Some(n) if n == cur => Err(format!("T{n} is in the spindle; unload it first")),
            Some(n) => st.idle().and_then(|_| {
                st.tools.retain(|x| x.t != n);
                st.selected = None;
                st.save().map(|_| n)
            }).map(|n| {
                history::forget_wear(n);
                st.note(format!("T{n} deleted"), false)
            }),
        };
        if let Err(e) = r {
            st.note(e, true);
        }
        st.refresh();
    });
    let t = tools.clone();
    actions.on_tools_create_missing(move || {
        let mut st = t.borrow_mut();
        let Some(ui) = st.ui.upgrade() else { return };
        let lines: Vec<slint::SharedString> = ui.global::<Live>().get_gcode_lines().iter().collect();
        let missing: Vec<i32> = program_tools(lines.iter().map(|s| s.as_str())).into_iter().filter(|n| *n > 0 && !st.tools.iter().any(|t| t.t == *n)).collect();
        let r = st.idle().and_then(|_| {
            for n in &missing {
                st.tools.push(Tool { t: *n, z: 0.0, d: 0.0, other: Vec::new(), comment: "Added for the loaded program".into() });
            }
            st.tools.sort_by_key(|t| t.t);
            st.save()
        });
        match r {
            Ok(()) => st.note(format!("Added {} tool(s); touch off each before cutting", missing.len()), false),
            Err(e) => st.note(e, true),
        }
        st.refresh();
    });
    let t = tools.clone();
    actions.on_tools_drawbar(move |extend| {
        let st = t.borrow();
        if let Some(io) = &st.io {
            let i = io.read();
            if extend && !(i.spindle_request == Some(false) && !i.change && st.status.as_ref().is_some_and(|s| !s.busy())) {
                st.note("The drawbar can only release with the spindle off and nothing running", true);
            } else if !extend && i.spindle_request != Some(false) {
                st.note("Turn the spindle command off (M5) before clamping: it would start as the drawbar retracts", true);
            } else {
                io.set_manual_request(extend);
                // With the drawbar released the tool comes out: clear the
                // logical tool too (as Probe Basic's drawbar button did).
                if extend && st.status.as_ref().is_some_and(|s| s.tool_in_spindle > 0) {
                    st.machine.send(Command::Mdi("o<unload_spindle> call".into()));
                }
            }
        }
        st.refresh();
    });
    let t = tools.clone();
    actions.on_tools_reload(move || {
        let mut st = t.borrow_mut();
        st.load();
        st.refresh();
    });
    tools
}

thread_local! {
    static LOADED: std::cell::Cell<Option<(f64, f64)>> = const { std::cell::Cell::new(None) };
}

/// The tool in the spindle as (diameter, length) from the tool table.
pub fn loaded_tool() -> Option<(f64, f64)> {
    LOADED.with(|l| l.get())
}

/// Status update: the current tool and the loaded program, plus a reload
/// when a command finishes (LinuxCNC rewrites the table on G10 L1/L10 and
/// touch off).
pub fn apply_status(ui: &MainWindow, tools: &Rc<RefCell<Tools>>, s: &Status) {
    let mut st = tools.borrow_mut();
    let changed = st.status.as_ref().is_none_or(|o| o.tool_in_spindle != s.tool_in_spindle || o.busy() != s.busy() || o.file != s.file);
    st.status = Some(s.clone());
    if changed {
        st.load();
        st.show(ui);
        let tool = st.tools.iter().find(|t| t.t == s.tool_in_spindle && s.tool_in_spindle > 0);
        LOADED.with(|l| l.set(tool.map(|t| (t.d, t.z))));
        let dia = tool.map(|t| format!("{:.3}", t.d));
        ui.global::<Live>().set_tool_dia(dia.unwrap_or_else(|| "-".into()).into());
    }
}

/// Refresh the drawbar state (HAL inputs change without a status event).
pub fn start_timer(ui: &MainWindow, tools: &Rc<RefCell<Tools>>) -> slint::Timer {
    let timer = slint::Timer::default();
    let (t, weak) = (tools.clone(), ui.as_weak());
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(250), move || {
        if let Some(ui) = weak.upgrade() {
            t.borrow().show_drawbar(&ui);
        }
    });
    timer
}


#[cfg(test)]
mod tests {
    use super::*;

    const TBL: &str = ";Tool  Pocket Z Offset     Diameter     Remark\nT1     P1     Z+144.438333 D+1.000000   ; Jobber 1.0\nT99 P99 Z+125.673 D+3.175 ;Probe\nT12 P12 Z+0 D+6.35\n";

    #[test]
    fn parse_and_format_round_trip() {
        let t = parse(TBL);
        assert_eq!(t.iter().map(|t| t.t).collect::<Vec<_>>(), [1, 12, 99]);
        assert_eq!(t[0].z, 144.438333);
        assert_eq!(t[0].comment, "Jobber 1.0");
        assert_eq!(t[0].other, ["P1"]);
        assert_eq!(parse(&format(&t)), t);
    }

    #[test]
    fn wear_text_is_signed_and_empty_for_none() {
        assert_eq!(wear_text(0.0), "");
        assert_eq!(wear_text(-0.02), "-0.020");
        assert_eq!(wear_text(0.015), "+0.015");
    }

    #[test]
    fn program_tool_words() {
        let lines = ["T14 M6", "(T5 in a comment)", "M6 T 23 ; T7 too", "G0 X1", "T14"];
        assert_eq!(program_tools(lines.into_iter()), [14, 23]);
    }
}
