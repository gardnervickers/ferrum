//! UTIL screen (milestone 5): spindle warm-up, the tool setter (air blast,
//! G30 position, spindle nose reference) and a machine status grid.
//!
//! Warm-up follows custom_ui/tabs/util/util.py: confirm the holder, then
//! stages of RPM for minutes. Each tick checks that the spindle still runs
//! forward at the stage's commanded RPM with the override at 100 %; anything
//! else stops it. The spindle-off command is sent on every exit path, and the
//! operator is told to verify the coast-down (the command is all we know).

use crate::config::Config;
use crate::modules::SetterHal;
use crate::settings::Settings;
use crate::toolchange::Io;
use crate::{Actions, Live, MainWindow, StatusRow, WarmStage};
use machine::{Command, Handle, Message, Rotation, SpindleDirection, Status, TaskState};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const DEFAULT_STAGES: [(f64, f64); 4] = [(1000.0, 3.0), (2000.0, 3.0), (3000.0, 3.0), (4000.0, 3.0)];

struct Warmup {
    stage: usize,
    stage_start: Instant,
    started: Instant,
}

pub struct Util {
    stages: Vec<(f64, f64)>,
    secure: bool,
    warm: Option<Warmup>,
    last_note: String,
    g30: [Option<f64>; 3],
    status: Option<Status>,
    probe_tool: i32,
    machine: Handle,
    io: Option<Rc<dyn Io>>,
    setter: Option<Rc<SetterHal>>,
    settings: Settings,
    ui: slint::Weak<MainWindow>,
    blast_until: Option<Instant>,
}

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

impl Util {
    fn total(&self) -> f64 {
        self.stages.iter().map(|(_, m)| m * 60.0).sum()
    }

    fn note(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.last_note = text.clone();
        if let Some(ui) = self.ui.upgrade() {
            let live = ui.global::<Live>();
            live.set_message(text.into());
            live.set_message_error(error);
        }
    }

    /// Why a warm-up can't start now.
    fn warm_ready(&self) -> Result<(), String> {
        let s = self.status.as_ref().ok_or("Not connected to LinuxCNC")?;
        if !s.power_on() {
            return Err("Turn the machine on".into());
        }
        if s.busy() {
            return Err("Wait for the running command to finish".into());
        }
        if s.tool_in_spindle == self.probe_tool {
            return Err("Unload the probe before a warm-up".into());
        }
        if s.spindle_enabled {
            return Err("The spindle must be stopped".into());
        }
        if let Some(io) = &self.io {
            if io.read().drawbar_extended != Some(false) {
                return Err("The drawbar must be clamped".into());
            }
        }
        if let Some(ui) = self.ui.upgrade() {
            let block = ui.global::<Live>().get_spindle_block();
            if !block.is_empty() {
                return Err(block.to_string());
            }
        }
        if !self.secure {
            return Err("Confirm the tool holder is secure".into());
        }
        if self.stages.iter().any(|(r, m)| *r <= 0.0 || *m <= 0.0) {
            return Err("Every stage needs an RPM and minutes".into());
        }
        Ok(())
    }

    fn start(&mut self) {
        if let Err(e) = self.warm_ready() {
            self.note(e, true);
            return;
        }
        self.machine.send(Command::SpindleScale(1.0));
        self.machine.send(Command::SpindleOn { rpm: self.stages[0].0, rotation: Rotation::Forward });
        let now = Instant::now();
        self.warm = Some(Warmup { stage: 0, stage_start: now, started: now });
        self.note(format!("Warm-up stage 1: {} rpm", self.stages[0].0), false);
    }

    fn stop(&mut self, why: &str, error: bool) {
        if self.warm.take().is_some() {
            self.machine.send(Command::SpindleOff);
            self.secure = false;
            self.note(format!("{why}; spindle-off command sent. Verify coast-down."), error);
        }
    }

    fn tick(&mut self) {
        if let Some(t) = self.blast_until {
            if Instant::now() >= t {
                self.blast_until = None;
                if let Some(h) = &self.setter {
                    h.blast_request.set(false);
                }
            }
        }
        let Some(w) = &self.warm else { return };
        let Some(s) = self.status.clone() else { return };
        let rpm = self.stages[w.stage].0;
        let grace = w.stage_start.elapsed() < Duration::from_millis(1500);
        let wrong = if s.task_state != TaskState::On {
            Some("Machine turned off")
        } else if s.tool_in_spindle == self.probe_tool {
            Some("Probe in spindle")
        } else if !grace && !s.spindle_enabled {
            Some("Spindle stopped during warm-up")
        } else if !grace && (s.spindle_direction != SpindleDirection::Forward || (s.spindle_speed - rpm).abs() > 0.5) {
            Some("Spindle RPM or direction changed")
        } else if !grace && (s.spindle_scale - 1.0).abs() > 1e-6 {
            Some("Spindle override changed")
        } else {
            None
        };
        if let Some(why) = wrong {
            self.stop(&format!("Warm-up stopped: {why}"), true);
            return;
        }
        if w.stage_start.elapsed().as_secs_f64() >= self.stages[w.stage].1 * 60.0 {
            let next = w.stage + 1;
            if next >= self.stages.len() {
                self.stop("Warm-up complete", false);
            } else {
                let rpm = self.stages[next].0;
                self.machine.send(Command::SpindleOn { rpm, rotation: Rotation::Forward });
                self.warm = Some(Warmup { stage: next, stage_start: Instant::now(), started: w.started });
                self.note(format!("Warm-up stage {}: {rpm} rpm", next + 1), false);
            }
        }
    }

    fn show(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        let stages: Vec<WarmStage> = self
            .stages
            .iter()
            .enumerate()
            .map(|(i, (r, m))| WarmStage {
                rpm: format!("{r:.0}").into(),
                minutes: format!("{m}").into(),
                state: match &self.warm {
                    Some(w) if w.stage == i => "run",
                    Some(w) if w.stage > i => "done",
                    _ => "",
                }
                .into(),
            })
            .collect();
        crate::models::sync(live.get_wu_stages(), stages, |m| live.set_wu_stages(m));
        live.set_wu_secure(self.secure);
        live.set_wu_running(self.warm.is_some());
        let total = self.total();
        let (done, text) = match &self.warm {
            Some(w) => {
                let el = w.started.elapsed().as_secs_f64();
                let rpm = self.status.as_ref().map_or(0.0, |s| if s.spindle_enabled { s.spindle_speed } else { 0.0 });
                (el / total, format!("STAGE {} · RPM CMD {rpm:.0} · {} / {}", w.stage + 1, mmss(el), mmss(total)))
            }
            None => (0.0, format!("NOT STARTED · 00:00 / {}", mmss(total))),
        };
        live.set_wu_progress(done.clamp(0.0, 1.0) as f32);
        live.set_wu_text(text.into());
        live.set_wu_note(format!("{} stages, {:.0} to {:.0} rpm, {} total. Confirm the holder is secure first.", self.stages.len(), self.stages.first().map_or(0.0, |s| s.0), self.stages.last().map_or(0.0, |s| s.0), mmss(total)).into());
        live.set_wu_ready(self.warm_ready().err().unwrap_or_default().into());
        // Tool setter
        let nose = self.io.as_ref().and_then(|io| io.read().nose_reference);
        live.set_ut_nose(match nose { Some(true) => "VALID", Some(false) => "NEEDS PROBE", None => "UNKNOWN" }.into());
        live.set_ut_nose_kind(match nose { Some(true) => "good", Some(false) => "warn", None => "" }.into());
        live.set_ut_setter(match &self.setter { Some(h) if h.triggered.get() => "TRIPPED", Some(_) => "READY", None => "NOT FITTED" }.into());
        live.set_ut_has_blast(self.setter.is_some());
        live.set_ut_blast(self.setter.as_ref().is_some_and(|h| h.blast_active.get()));
        let g30 = |v: Option<f64>| v.map_or("-".to_string(), |v| crate::machine_info::get().units.fmt(v));
        live.set_ut_g30(format!("X {}  Y {}  Z {}", g30(self.g30[0]), g30(self.g30[1]), g30(self.g30[2])).into());
        // Machine status
        let mut rows: Vec<StatusRow> = Vec::new();
        let mut row = |k: &str, v: String, kind: &str| rows.push(StatusRow { key: k.into(), value: v.into(), kind: kind.into() });
        match &self.status {
            None => row("Machine", "DISCONNECTED".into(), "bad"),
            Some(s) => {
                row("Machine", if s.estop() { "E-STOP" } else if s.power_on() { "ON" } else { "OFF" }.into(), if s.power_on() { "good" } else { "bad" });
                row("Mode", format!("{:?}", s.mode).to_uppercase(), "");
                row("Interpreter", format!("{:?}", s.interp).to_uppercase(), "");
                row("Homed", if s.all_homed() { "ALL".into() } else { "NO".into() }, if s.all_homed() { "good" } else { "warn" });
                row("Tool", if s.tool_in_spindle == 0 { "EMPTY".into() } else { format!("T{}", s.tool_in_spindle) }, "");
                row("Spindle", match (s.spindle_enabled, s.spindle_direction) { (false, _) => "STOPPED".into(), (_, SpindleDirection::Forward) => "FORWARD".into(), (_, SpindleDirection::Reverse) => "REVERSE".into(), _ => "ON".into() }, "");
                row("Speed cmd", format!("{:.0} RPM", if s.spindle_enabled { s.spindle_speed * s.spindle_scale } else { 0.0 }), "");
                row("WCS", s.wcs.name().into(), "");
            }
        }
        if let Some(io) = &self.io {
            let i = io.read();
            row("Drawbar", match i.drawbar_extended { Some(true) => "EXTENDED", Some(false) => "RETRACTED", None => "UNKNOWN" }.into(), if i.drawbar_extended == Some(true) { "warn" } else { "" });
            row("Nose ref", match i.nose_reference { Some(true) => "VALID", Some(false) => "NEEDS PROBE", None => "UNKNOWN" }.into(), if i.nose_reference == Some(true) { "good" } else { "warn" });
        }
        if let Some(h) = &self.setter {
            row("Tool setter", if h.triggered.get() { "TRIPPED" } else { "READY" }.into(), if h.triggered.get() { "warn" } else { "good" });
            row("Setter blast", if h.blast_active.get() { "ON" } else { "OFF" }.into(), "");
        }
        crate::models::sync(live.get_ut_status(), rows, |m| live.set_ut_status(m));
    }

    fn refresh(&self) {
        if let Some(ui) = self.ui.upgrade() {
            self.show(&ui);
        }
    }

    fn mdi(&mut self, lines: &[String]) {
        match &self.status {
            Some(s) if s.power_on() && !s.busy() && s.all_homed() => lines.iter().for_each(|l| self.machine.send(Command::Mdi(l.clone()))),
            _ => self.note("Machine on, homed and idle first", true),
        }
    }

    fn save_stages(&mut self) {
        let list: Vec<String> = self.stages.iter().map(|(r, m)| format!("{r},{m}")).collect();
        self.settings.set_list("warmup.stage.", list);
    }
}

struct Keys(Rc<RefCell<Util>>);

impl crate::manual::KeypadTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        let (k, i) = what.strip_prefix("wu-")?.split_once('-')?;
        Some(format!("STAGE {} {}", i.parse::<usize>().ok()? + 1, if k == "rpm" { "RPM" } else { "MINUTES" }))
    }
    fn current(&self, what: &str) -> Option<f64> {
        let (k, i) = what.strip_prefix("wu-")?.split_once('-')?;
        let s = *self.0.borrow().stages.get(i.parse::<usize>().ok()?)?;
        Some(if k == "rpm" { s.0 } else { s.1 })
    }
    fn apply(&self, what: &str, v: f64) -> Option<Result<(), String>> {
        let (k, i) = what.strip_prefix("wu-")?.split_once('-')?;
        let i = i.parse::<usize>().ok()?;
        let mut u = self.0.borrow_mut();
        let r = if u.warm.is_some() {
            Err("Stop the warm-up to change its stages".to_string())
        } else if !(v > 0.0) || (k == "rpm" && v > 24000.0) || (k == "min" && v > 120.0) {
            Err("Out of range".to_string())
        } else {
            if k == "rpm" { u.stages[i].0 = v.round() } else { u.stages[i].1 = v }
            u.save_stages();
            Ok(())
        };
        u.refresh();
        Some(r)
    }
}

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, io: Option<Rc<dyn Io>>, setter: Option<Rc<SetterHal>>, settings: Settings) -> (Rc<RefCell<Util>>, slint::Timer) {
    let probe_tool = cfg.get("PROBE", "TOOL_NUMBER").and_then(|v| v.trim().parse().ok()).unwrap_or(99);
    let mut stages: Vec<(f64, f64)> = settings
        .list("warmup.stage.")
        .iter()
        .filter_map(|v| v.split_once(',').and_then(|(r, m)| Some((r.parse().ok()?, m.parse().ok()?))))
        .collect();
    if stages.len() != DEFAULT_STAGES.len() {
        stages = DEFAULT_STAGES.to_vec();
    }
    // G30 (#5181–5183) from the parameter file; SET G30 HERE updates it.
    let mut g30 = [None; 3];
    let path = cfg.dir().join(cfg.get("RS274NGC", "PARAMETER_FILE").unwrap_or("linuxcnc.var"));
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let mut w = line.split_whitespace();
            if let (Some(Ok(n)), Some(Ok(v))) = (w.next().map(str::parse::<usize>), w.next().map(str::parse::<f64>)) {
                if (5181..=5183).contains(&n) {
                    g30[n - 5181] = Some(v);
                }
            }
        }
    }
    let util = Rc::new(RefCell::new(Util { stages, secure: false, warm: None, last_note: String::new(), g30, status: None, probe_tool, machine: machine.clone(), io, setter, settings, ui: ui.as_weak(), blast_until: None }));
    crate::manual::add_keypad_target(Rc::new(Keys(util.clone())));
    let actions = ui.global::<Actions>();
    let u = util.clone();
    actions.on_wu_secure(move || {
        let mut u = u.borrow_mut();
        if u.warm.is_none() {
            u.secure = !u.secure;
        }
        u.refresh();
    });
    let u = util.clone();
    actions.on_wu_start(move || {
        let mut u = u.borrow_mut();
        u.start();
        u.refresh();
    });
    let u = util.clone();
    actions.on_wu_stop(move || {
        let mut u = u.borrow_mut();
        u.stop("Warm-up stopped", false);
        u.refresh();
    });
    let u = util.clone();
    actions.on_ut_blast(move || {
        let mut u = u.borrow_mut();
        if let Some(h) = &u.setter {
            h.blast_request.set(true);
            u.blast_until = Some(Instant::now() + Duration::from_millis(50));
        }
    });
    let u = util.clone();
    actions.on_ut_go_g30(move || u.borrow_mut().mdi(&go_g30_lines()));
    let u = util.clone();
    actions.on_ut_set_g30(move || u.borrow_mut().mdi(&set_g30_lines()));
    let u = util.clone();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        let mut x = u.borrow_mut();
        x.tick();
        x.refresh();
    });
    util.borrow().refresh();
    (util, timer)
}

pub fn apply_status(util: &Rc<RefCell<Util>>, s: &Status) {
    util.borrow_mut().status = Some(s.clone());
}

/// GO TO G30: Z to the top of travel first, then G30 (which moves through
/// the current position, so Z must be clear).
pub fn go_g30_lines() -> Vec<String> {
    vec![machine::keep_modes(&crate::machine_info::get().z_top_line()), "G30".into()]
}

/// SET G30 HERE: G30.1 stores the position, and a DEBUG line reports it
/// back (`message`).
pub fn set_g30_lines() -> Vec<String> {
    vec!["G30.1".into(), format!("(DEBUG,{G30_REPORT} #5181 #5182 #5183)")]
}

const G30_REPORT: &str = "millgui G30";

/// The G30 position as reported after SET G30 (or by set_g30_position.ngc,
/// as DEBUG EVAL lines).
pub fn message(util: &Rc<RefCell<Util>>, m: &Message) -> bool {
    if let Some(rest) = m.text.trim().strip_prefix(G30_REPORT) {
        let v: Vec<f64> = rest.split_whitespace().filter_map(|w| w.parse().ok()).collect();
        if v.len() != 3 {
            return false;
        }
        let mut u = util.borrow_mut();
        u.g30 = [Some(v[0]), Some(v[1]), Some(v[2])];
        u.note("G30 tool setter position saved", false);
        return true;
    }
    let Some((name, v)) = crate::probe::parse_eval(&m.text) else { return false };
    let i = match name.as_str() {
        "x_tool_change_position" => 0,
        "y_tool_change_position" => 1,
        "z_tool_change_position" => 2,
        _ => return false,
    };
    let mut u = util.borrow_mut();
    u.g30[i] = v.first().copied();
    if i == 2 {
        u.note("G30 tool setter position saved", false);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock() {
        assert_eq!(mmss(725.0), "12:05");
        assert_eq!(mmss(-1.0), "00:00");
    }
}
