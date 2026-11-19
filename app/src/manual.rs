//! Manual control: jog, spindle, mist, work zero, power feed, the keypad and the
//! message log. Everything here is locked in the UI while a program runs or
//! is paused (`Live.manual-locked`); LinuxCNC enforces its own mode rules.

use crate::calc;
use crate::config::Config;
use crate::machine_info::{self, Step};
use crate::powerfeed::{self, Mode, PowerFeed};
use crate::settings::Settings;
use crate::{Actions, GotoRow, Live, MainWindow, MsgRow};
use machine::{Command, Handle, Interp, JogTarget, JointSel, Message, MessageKind, Rotation, Status};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const LOG: usize = 300;

struct Range {
    min: f64,
    max: f64,
}

pub struct Manual {
    /// The selected step's label ("CONT" for continuous).
    step: String,
    /// [DISPLAY]INCREMENTS, read once.
    steps: Vec<Step>,
    speed: f64,
    speed_a: f64,
    rpm: f64,
    linear: Range,
    angular: Range,
    keypad: Option<Pad>,
    status: Option<Status>,
    pf: PowerFeed,
    settings: Settings,
    /// Last power feed / GO TO message, and whether it is an error.
    pf_note: Option<(String, bool)>,
    log: Rc<VecModel<MsgRow>>,
}

/// The open keypad.
#[derive(Clone)]
struct Pad {
    /// The value it edits ("jog-speed", "off-x", ...).
    what: String,
    /// The typed text, a number or an expression (see `calc`).
    buf: String,
    /// The text is still the old value: a digit replaces it, an operator
    /// works on it, as on a calculator.
    fresh: bool,
    /// Why ENTER was refused; cleared by the next key.
    error: Option<String>,
}

impl Pad {
    fn key(&mut self, k: &str) {
        // Hardware keyboard spellings.
        let k = match k {
            "*" | "x" | "X" => "×",
            "/" => "÷",
            "-" => "−",
            k => k,
        };
        let digit = k.len() == 1 && k.chars().all(|c| c.is_ascii_digit());
        if !digit && !matches!(k, "." | "⌫" | "CLR" | "±" | "(" | ")") && !(k.chars().count() == 1 && OPS.contains(k)) {
            return;
        }
        self.error = None;
        // The old value is selected: a digit, '.', '(' or backspace
        // replaces it; an operator or ± works on it.
        if std::mem::take(&mut self.fresh) && (digit || matches!(k, "." | "⌫" | "(")) {
            self.buf.clear();
            if k == "⌫" {
                return;
            }
        }
        key(&mut self.buf, k);
    }
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Manual {
    fn from_config(cfg: &Config, settings: Settings) -> Self {
        let mi = machine_info::get();
        let f = |k: &str, d: f64| cfg.get_f64("DISPLAY", k).unwrap_or(d);
        // Jog speeds are machine units per second. Without [DISPLAY] values:
        // the trajectory's top speed, else the slowest linear axis.
        let axis_max = ["X", "Y", "Z"].iter().filter_map(|a| cfg.get_f64(&format!("AXIS_{a}"), "MAX_VELOCITY")).reduce(f64::min);
        let top = cfg.get_f64("TRAJ", "MAX_LINEAR_VELOCITY").or(axis_max).unwrap_or(mi.mm(100.0));
        let linear = Range { min: f("MIN_LINEAR_VELOCITY", mi.mm(0.01)), max: f("MAX_LINEAR_VELOCITY", top) };
        let (steps, _) = machine_info::jog_steps(cfg.get("DISPLAY", "INCREMENTS"), mi.units);
        Self {
            step: "CONT".into(),
            steps,
            speed: settings.f64("jog.speed", f("DEFAULT_LINEAR_VELOCITY", (linear.max / 2.0).min(mi.units.pick(50.0, 2.0)))),
            speed_a: settings.f64("jog.speed-a", f("DEFAULT_ANGULAR_VELOCITY", 180.0)),
            rpm: settings.f64("powerfeed.spindle-rpm", 300.0),
            pf: PowerFeed::new(cfg, &settings),
            settings,
            pf_note: None,
            linear,
            angular: Range { min: f("MIN_ANGULAR_VELOCITY", 0.01), max: f("MAX_ANGULAR_VELOCITY", 180.0) },
            keypad: None,
            status: None,
            log: Rc::new(VecModel::default()),
        }
    }

    fn ready(&self, live: &Live) -> Result<(), String> {
        match &self.status {
            Some(s) => powerfeed::ready(s, live.get_motion_block().as_str()),
            None => Err("LinuxCNC status unavailable".into()),
        }
    }

    fn show(&self, live: &Live) {
        let pf = &self.pf;
        live.set_pf_mode(if pf.mode == Mode::EndOfTravel { "eot" } else { "dist" }.into());
        live.set_pf_feed(fmt(pf.feed).into());
        live.set_pf_dist(fmt(pf.dist).into());
        live.set_pf_wcs(pf.wcs_code().into());
        crate::models::sync(live.get_pf_axis_enabled(), (0..3).map(|a| pf.axis_enabled(a)).collect::<Vec<_>>(), |m| live.set_pf_axis_enabled(m));
        live.set_gt_x(format!("{:.3}", pf.gt_x).into());
        live.set_gt_y(format!("{:.3}", pf.gt_y).into());
        live.set_gt_feed(fmt(pf.gt_feed).into());
        let (pts, target, origin) = pf.preview();
        let rows: Vec<GotoRow> = pf
            .gotos
            .iter()
            .zip(pts)
            .map(|(g, (px, py))| GotoRow {
                name: g.name.clone().into(),
                x: format!("{:.3}", g.x).into(),
                y: format!("{:.3}", g.y).into(),
                px,
                py,
            })
            .collect();
        crate::models::sync(live.get_gotos(), rows, |m| live.set_gotos(m));
        live.set_goto_selected(pf.selected.map_or(-1, |i| i as i32));
        live.set_gt_px(target.0);
        live.set_gt_py(target.1);
        live.set_gt_ox(origin.0);
        live.set_gt_oy(origin.1);
        let ready = self.ready(live);
        let busy = self.status.as_ref().is_some_and(|s| s.busy());
        live.set_pf_ready(ready.is_ok());
        let (text, error) = match (&self.pf_note, &ready) {
            (Some((t, e)), _) if busy || ready.is_ok() || *e => (t.clone(), *e),
            (_, Err(reason)) => (reason.clone(), true),
            _ => (format!("Ready · moves run in {}", pf.wcs_code()), false),
        };
        live.set_pf_status(text.into());
        live.set_pf_status_error(error);

        live.set_jog_step(self.step.clone().into());
        live.set_jog_speed(fmt(self.speed).into());
        live.set_jog_speed_a(fmt(self.speed_a).into());
        live.set_spindle_rpm(fmt(self.rpm).into());
        live.set_keypad_open(self.keypad.is_some());
        live.set_keypad_fresh(self.keypad.as_ref().is_some_and(|k| k.fresh));
        if let Some(pad) = &self.keypad {
            let what = &pad.what;
            let l = KEYPAD.with(|t| t.borrow().iter().find_map(|t| t.label(what))).unwrap_or_else(|| label(what));
            live.set_keypad_label(l.into());
            live.set_keypad_value(pad.buf.clone().into());
            let (result, error) = preview(pad);
            live.set_keypad_result(result.into());
            live.set_keypad_error(error);
        }
    }

    /// Range check for keypad entries; the error goes to the message line.
    fn apply(&mut self, what: &str, v: f64) -> Result<(), String> {
        let check = |v: f64, r: &Range, unit: &str| {
            if v < r.min || v > r.max {
                Err(format!("{} must be {}–{} {unit}", label(what), fmt(r.min), fmt(r.max)))
            } else {
                Ok(v)
            }
        };
        match what {
            "jog-speed" => self.speed = check(v, &self.linear, &format!("{}/s", machine_info::get().units.name()))?,
            "jog-speed-a" => self.speed_a = check(v, &self.angular, "°/s")?,
            "spindle-rpm" => {
                if v <= 0.0 {
                    return Err("RPM must be above 0".into());
                }
                if let Some(max) = machine_info::get().max_rpm.filter(|m| v > *m) {
                    return Err(format!("The spindle's top speed is {max:.0} rpm"));
                }
                self.rpm = v;
            }
            "pf-feed" | "gt-feed" if v <= 0.0 => return Err("Feed must be greater than zero".into()),
            "pf-feed" => self.pf.feed = v,
            "gt-feed" => self.pf.gt_feed = v,
            "pf-dist" if v <= 0.0 => return Err("Distance must be greater than zero".into()),
            "pf-dist" => self.pf.dist = v,
            "gt-x" => self.pf.gt_x = v,
            "gt-y" => self.pf.gt_y = v,
            _ => {
                return match KEYPAD.with(|t| t.borrow().iter().find_map(|t| t.apply(what, v))) {
                    Some(r) => r,
                    None => Ok(()),
                }
            }
        }
        self.persist();
        Ok(())
    }

    fn persist(&mut self) {
        self.settings.set("jog.speed", self.speed);
        self.settings.set("jog.speed-a", self.speed_a);
        self.settings.set("powerfeed.spindle-rpm", self.rpm);
        self.pf.save(&mut self.settings);
    }

    fn current(&self, what: &str) -> f64 {
        match what {
            "jog-speed" => self.speed,
            "jog-speed-a" => self.speed_a,
            "spindle-rpm" => self.rpm,
            "pf-feed" => self.pf.feed,
            "pf-dist" => self.pf.dist,
            "gt-x" => self.pf.gt_x,
            "gt-y" => self.pf.gt_y,
            "gt-feed" => self.pf.gt_feed,
            _ => KEYPAD.with(|t| t.borrow().iter().find_map(|t| t.current(what))).unwrap_or(0.0),
        }
    }
}

/// Another screen's values edited on the shared keypad (Tools, Offsets).
/// Each answers only for the names it owns (`None` otherwise).
pub trait KeypadTarget {
    fn label(&self, what: &str) -> Option<String>;
    fn current(&self, what: &str) -> Option<f64>;
    /// Range-check and store; `Err` keeps the keypad open with the message.
    fn apply(&self, what: &str, v: f64) -> Option<Result<(), String>>;
}

thread_local! {
    static KEYPAD: RefCell<Vec<Rc<dyn KeypadTarget>>> = const { RefCell::new(Vec::new()) };
}

pub fn add_keypad_target(t: Rc<dyn KeypadTarget>) {
    KEYPAD.with(|k| k.borrow_mut().push(t));
}

fn label(what: &str) -> String {
    let mi = machine_info::get();
    let u = mi.units.upper();
    match what {
        "jog-speed" => format!("JOG SPEED {u}/S"),
        "jog-speed-a" => format!("{} SPEED °/S", mi.rotary().map_or('A', |a| a.letter)),
        "spindle-rpm" => "SPINDLE RPM".into(),
        "pf-feed" | "gt-feed" => format!("FEED {u}/MIN"),
        "pf-dist" => format!("DISTANCE {u}"),
        "gt-x" => "TARGET X (WORK)".into(),
        "gt-y" => "TARGET Y (WORK)".into(),
        _ => "VALUE".into(),
    }
}

/// The line under the keypad's value: the refusal after ENTER, otherwise
/// the result of an expression as it is typed (nothing for a plain number
/// or an unfinished one).
fn preview(pad: &Pad) -> (String, bool) {
    if let Some(e) = &pad.error {
        return (e.clone(), true);
    }
    if !calc::is_expression(&pad.buf) {
        return (String::new(), false);
    }
    match calc::eval(&pad.buf) {
        Ok(v) => (format!("= {}", fmt_result(v)), false),
        Err(_) => (String::new(), false),
    }
}

/// A result to four places, so 1/3 shows as 0.3333 rather than a long tail.
fn fmt_result(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.replace('-', "−") }
}

const OPS: &str = "+−×÷";
/// Longest entry; room for "(12.5-0.25)/2+100.125" and the like.
const MAX_LEN: usize = 32;

/// One keypad key. Keys that would make the text unreadable as a number or
/// expression (a second '.', ')' with nothing open, a digit after ')') do
/// nothing; an operator after an operator replaces it.
fn key(buf: &mut String, k: &str) {
    let last = buf.chars().last();
    let after_op = last.is_none_or(|c| OPS.contains(c) || c == '(');
    // Start of the number being typed at the end of the text.
    let num_start = buf.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.').len();
    match k {
        "⌫" => {
            buf.pop();
        }
        "CLR" => buf.clear(),
        "±" => {
            // Flip the sign of the last number, or of what comes next.
            let before = &buf[..num_start];
            let unary = before.ends_with('−') && before[..before.len() - '−'.len_utf8()].chars().last().is_none_or(|c| OPS.contains(c) || c == '(');
            if unary {
                buf.replace_range(num_start - '−'.len_utf8()..num_start, "");
            } else if num_start < buf.len() || after_op {
                buf.insert(num_start, '−');
            }
        }
        _ if buf.len() >= MAX_LEN => {}
        "." if buf[num_start..].contains('.') || last == Some(')') => {}
        "." if num_start == buf.len() => buf.push_str("0."),
        "." => buf.push('.'),
        "(" if after_op => buf.push('('),
        ")" if buf.matches('(').count() > buf.matches(')').count() && !after_op => buf.push(')'),
        op if op.chars().count() == 1 && OPS.contains(op) => {
            let base = buf.trim_end_matches(|c| OPS.contains(c)).len();
            if op == "−" && matches!(last, Some('×' | '÷')) {
                // "2×−3": a minus after × or ÷ is a sign.
                buf.push('−');
            } else if base == 0 || buf[..base].ends_with('(') {
                // Only a sign can start a number.
                if op == "−" {
                    buf.truncate(base);
                    buf.push('−');
                }
            } else {
                // An operator after an operator replaces it.
                buf.truncate(base);
                buf.push_str(op);
            }
        }
        d if d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()) && last != Some(')') => buf.push_str(d),
        _ => {}
    }
}

/// Programs and MDI must be idle for manual control.
pub fn locked(s: &Status) -> bool {
    s.interp != Interp::Idle
}

pub fn apply_status(ui: &MainWindow, manual: &Rc<RefCell<Manual>>, s: &Status) {
    let live = ui.global::<Live>();
    live.set_manual_locked(locked(s));
    live.set_mist(s.mist);
    live.set_flood(s.flood);
    let mut m = manual.borrow_mut();
    m.status = Some(s.clone());
    m.show(&live);
}

/// Re-evaluate readiness when module state (drives, interlock) changes.
pub fn refresh(ui: &MainWindow, manual: &Rc<RefCell<Manual>>) {
    manual.borrow().show(&ui.global::<Live>());
}

pub fn log_message(ui: &MainWindow, manual: &Rc<RefCell<Manual>>, m: &Message) {
    let kind = match m.kind {
        MessageKind::Error => "error",
        MessageKind::Display => "display",
        MessageKind::Text => "text",
    };
    let log = manual.borrow().log.clone();
    log.insert(
        0,
        MsgRow {
            time: chrono::Local::now().format("%H:%M:%S").to_string().into(),
            text: m.text.trim().into(),
            kind: kind.into(),
        },
    );
    while log.row_count() > LOG {
        log.remove(LOG);
    }
    let _ = ui;
}

/// A message from the screens themselves, unless it repeats the newest row.
pub fn log_text(manual: &Rc<RefCell<Manual>>, text: &str, error: bool) {
    let log = manual.borrow().log.clone();
    if log.row_data(0).is_some_and(|r| r.text.as_str() == text.trim()) {
        return;
    }
    log.insert(0, MsgRow { time: chrono::Local::now().format("%H:%M:%S").to_string().into(), text: text.trim().into(), kind: if error { "error" } else { "text" }.into() });
    while log.row_count() > LOG {
        log.remove(LOG);
    }
}

fn note(ui: &MainWindow, text: impl Into<SharedString>, error: bool) {
    let live = ui.global::<Live>();
    live.set_message(text.into());
    live.set_message_error(error);
}

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, settings: Settings, has_a: bool) -> Rc<RefCell<Manual>> {
    let manual = Rc::new(RefCell::new(Manual::from_config(cfg, settings)));
    let actions = ui.global::<Actions>();
    let live = ui.global::<Live>();
    {
        let m = manual.borrow();
        live.set_messages(ModelRc::from(m.log.clone()));
        m.show(&live);
    }
    let mi = machine_info::get();
    live.set_has_a(has_a);
    live.set_unit(mi.units.name().into());
    live.set_unit_up(mi.units.upper().into());
    live.set_per_axis_home(mi.per_axis_home());
    live.set_home_all_ok(mi.home_all || !mi.per_axis_home());
    match mi.rotary() {
        Some(r) => {
            live.set_has_rot(true);
            live.set_rot_letter(r.letter.to_string().into());
            live.set_rot_axis(r.index as i32);
            live.set_rot_switch(r.home_switch);
        }
        None => live.set_has_rot(false),
    }
    live.set_spindle_reverse(machine_info::get().reverse);
    let steps: Vec<SharedString> = manual.borrow().steps.iter().map(|s| SharedString::from(s.label.as_str())).collect();
    crate::models::sync(live.get_jog_steps(), steps, |m| live.set_jog_steps(m));

    let (mc, mm) = (machine.clone(), manual.clone());
    actions.on_jog_start(move |axis, dir| {
        let m = mm.borrow();
        if m.status.as_ref().is_some_and(locked) {
            return;
        }
        let mi = machine_info::get();
        let rotary = mi.axis(axis as usize).is_some_and(|a| a.rotary);
        let speed = if rotary { m.speed_a } else { m.speed };
        let Some(target) = jog_target(axis) else { return };
        // A label that is not in the list never jogs (and never continuously).
        let Some(step) = m.steps.iter().find(|s| s.label == m.step) else { return };
        match step.size {
            // Steps are lengths; a rotary steps the same number in degrees.
            Some(incr) => mc.send(Command::JogIncr { target, vel: speed, incr: incr * dir as f64 }),
            None => mc.send(Command::JogCont { target, vel: speed * dir as f64 }),
        }
    });
    let mc = machine.clone();
    actions.on_jog_stop(move |axis| {
        if let Some(target) = jog_target(axis) {
            mc.send(Command::JogStop { target });
        }
    });

    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_jog_step_set(move |step| {
        let mut m = mm.borrow_mut();
        if !m.steps.iter().any(|s| s.label == step.as_str()) {
            return;
        }
        m.step = step.into();
        if let Some(ui) = weak.upgrade() {
            m.show(&ui.global::<Live>());
        }
    });

    // A rotary without a home switch: homing its joint with no search
    // velocity latches the current position as its home.
    let mc = machine.clone();
    actions.on_set_a_reference(move || {
        for j in machine_info::get().rotary().map(|r| r.joints.clone()).unwrap_or_default() {
            mc.send(Command::Home(JointSel::Joint(j as u8)));
        }
    });

    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_spindle(move |dir| {
        if dir != 0 {
            if let Some(ui) = weak.upgrade() {
                let block = ui.global::<Live>().get_spindle_block();
                if !block.is_empty() {
                    note(&ui, block.to_string(), true);
                    return;
                }
            }
        }
        let rpm = mm.borrow().rpm;
        mc.send(if dir == 0 { Command::SpindleOff } else { Command::SpindleOn { rpm, rotation: if dir < 0 { Rotation::Reverse } else { Rotation::Forward } } });
    });

    // NML coolant commands, not M7/M8/M9: they work mid-program, and
    // turning one off leaves the other alone.
    let coolant = |which: &'static str| {
        let (mc, mm) = (machine.clone(), manual.clone());
        move || {
            let (mist, flood) = mm.borrow().status.as_ref().map_or((false, false), |s| (s.mist, s.flood));
            mc.send(if which == "mist" { Command::Mist(!mist) } else { Command::Flood(!flood) });
        }
    };
    actions.on_mist_toggle(coolant("mist"));
    actions.on_flood_toggle(coolant("flood"));

    let mc = machine.clone();
    actions.on_zero_xyz(move || mc.send(Command::Mdi("G10 L20 P0 X0 Y0 Z0".into())));
    // Both run in the active WCS; the worker puts the modes back afterwards.
    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_z_to_work_zero(move || {
        let Some(ui) = weak.upgrade() else { return };
        let m = mm.borrow();
        match m.ready(&ui.global::<Live>()) {
            Ok(()) => mc.send(Command::Mdi(m.pf.z_zero_cmd())),
            Err(e) => note(&ui, e, true),
        }
    });
    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_retract_xy_to_zero(move || {
        let Some(ui) = weak.upgrade() else { return };
        let m = mm.borrow();
        let cmd = m.ready(&ui.global::<Live>()).and_then(|_| m.pf.retract_cmd(m.status.as_ref().unwrap(), machine_info::get().units.pick(10.0, 0.4)));
        match cmd {
            Ok(lines) => lines.into_iter().for_each(|c| mc.send(Command::Mdi(c))),
            Err(e) => note(&ui, e, true),
        }
    });

    // Park: Z to the top, then X and Y (and a rotary) to machine zero, the
    // moves Probe Basic's GO TO HOME macro made. Plain MDI, so it works
    // without that macro; the worker puts the modes back.
    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_park(move || {
        let Some(ui) = weak.upgrade() else { return };
        match mm.borrow().ready(&ui.global::<Live>()) {
            Ok(()) => {
                for line in park_lines(machine_info::get()) {
                    mc.send(Command::Mdi(line));
                }
            }
            Err(e) => note(&ui, e, true),
        }
    });

    wire_powerfeed(ui, machine, &manual);

    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_edit(move |what| {
        let mut m = mm.borrow_mut();
        let buf = fmt(m.current(&what)).replace('-', "−");
        m.keypad = Some(Pad { what: what.into(), buf, fresh: true, error: None });
        if let Some(ui) = weak.upgrade() {
            m.show(&ui.global::<Live>());
        }
    });
    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_keypad_key(move |k| {
        let mut m = mm.borrow_mut();
        if let Some(pad) = m.keypad.as_mut() {
            pad.key(&k);
        }
        if let Some(ui) = weak.upgrade() {
            m.show(&ui.global::<Live>());
        }
    });
    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_keypad_done(move |ok| {
        let Some(ui) = weak.upgrade() else { return };
        let mut m = mm.borrow_mut();
        if ok {
            if let Some(pad) = m.keypad.clone() {
                if let Err(e) = calc::eval(&pad.buf).and_then(|v| m.apply(&pad.what, v)) {
                    // Keep the keypad open, showing why, so the entry can
                    // be corrected; nothing is stored.
                    note(&ui, e.clone(), true);
                    if let Some(p) = m.keypad.as_mut() {
                        p.error = Some(e);
                    }
                    m.show(&ui.global::<Live>());
                    return;
                }
            }
        }
        m.keypad = None;
        m.show(&ui.global::<Live>());
    });

    let mm = manual.clone();
    actions.on_clear_messages(move || {
        let log = mm.borrow().log.clone();
        log.set_vec(Vec::new());
    });

    manual
}

/// The MDI lines of PARK.
pub fn park_lines(mi: &machine_info::MachineInfo) -> Vec<String> {
    let mut v = vec![machine::keep_modes(&mi.z_top_line())];
    let xy: String = ['X', 'Y'].iter().filter(|l| mi.has_axis(**l)).map(|l| format!(" {l}0")).collect();
    match crate::machine_cfg::get().machine.park {
        crate::machine_cfg::Park::G28 => {
            v.push(machine::keep_modes("G28"));
            return v;
        }
        crate::machine_cfg::Park::Xy(x, y) => {
            v.push(machine::keep_modes(&format!("G90 G53 G0 X{} Y{}", mi.units.gcode(x), mi.units.gcode(y))));
        }
        crate::machine_cfg::Park::MachineZero if !xy.is_empty() => v.push(machine::keep_modes(&format!("G90 G53 G0{xy}"))),
        crate::machine_cfg::Park::MachineZero => {}
    }
    let rot: String = mi.axes.iter().filter(|a| a.rotary).map(|a| format!(" {}0", a.letter)).collect();
    if !rot.is_empty() {
        v.push(machine::keep_modes(&format!("G90 G53 G0{rot}")));
    }
    v
}

fn wire_powerfeed(ui: &MainWindow, machine: &Handle, manual: &Rc<RefCell<Manual>>) {
    let actions = ui.global::<Actions>();
    // Each handler edits the power feed state, then the screen is redrawn
    // from it and the values are saved.
    let update = |f: Box<dyn Fn(&mut Manual, &Live)>| {
        let (mm, weak) = (manual.clone(), ui.as_weak());
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let live = ui.global::<Live>();
            let mut m = mm.borrow_mut();
            f(&mut m, &live);
            m.persist();
            m.show(&live);
        }
    };
    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_pf_mode_set(move |mode| {
        let Some(ui) = weak.upgrade() else { return };
        let mut m = mm.borrow_mut();
        m.pf.mode = if mode == "eot" { Mode::EndOfTravel } else { Mode::Distance };
        m.pf_note = None;
        m.persist();
        m.show(&ui.global::<Live>());
    });
    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_pf_wcs_set(move |code| {
        let Some(ui) = weak.upgrade() else { return };
        let mut m = mm.borrow_mut();
        if let Some(i) = powerfeed::WCS.iter().position(|c| *c == code.as_str()) {
            m.pf.wcs = i + 1;
        }
        m.pf_note = None;
        m.persist();
        m.show(&ui.global::<Live>());
    });
    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_pf_move(move |axis, dir| {
        let Some(ui) = weak.upgrade() else { return };
        let live = ui.global::<Live>();
        let mut m = mm.borrow_mut();
        let r = m.ready(&live).and_then(|_| m.pf.move_cmd(m.status.as_ref().unwrap(), axis as usize, dir));
        m.pf_note = Some(match r {
            Ok((cmd, msg)) => {
                mc.send(Command::Mdi(cmd));
                (msg, false)
            }
            Err(e) => (e, true),
        });
        m.show(&live);
    });
    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_goto_go(move || {
        let Some(ui) = weak.upgrade() else { return };
        let live = ui.global::<Live>();
        let mut m = mm.borrow_mut();
        let r = m.ready(&live).and_then(|_| m.pf.goto_cmd());
        m.pf_note = Some(match r {
            Ok((cmd, msg)) => {
                mc.send(Command::Mdi(cmd));
                (msg, false)
            }
            Err(e) => (e, true),
        });
        m.show(&live);
    });
    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_motion_stop(move || {
        mc.send(Command::Abort);
        if let Some(ui) = weak.upgrade() {
            let mut m = mm.borrow_mut();
            m.pf_note = Some(("Stopped".into(), false));
            m.show(&ui.global::<Live>());
        }
    });
    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_goto_select(move |i| {
        let Some(ui) = weak.upgrade() else { return };
        let mut m = mm.borrow_mut();
        m.pf.select(i.max(0) as usize);
        m.show(&ui.global::<Live>());
    });
    actions.on_goto_save(update(Box::new(|m, _| {
        m.pf.save_goto();
        m.pf_note = Some(("Saved WCS-relative GO TO target".into(), false));
    })));
    actions.on_goto_update(update(Box::new(|m, _| {
        m.pf.update_goto();
        m.pf_note = Some(("Updated the selected target".into(), false));
    })));
    actions.on_goto_delete(update(Box::new(|m, _| {
        m.pf.delete_goto();
        m.pf_note = Some(("Deleted the selected target".into(), false));
    })));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keys: &[&str]) -> String {
        let mut b = String::new();
        for k in keys {
            key(&mut b, k);
        }
        b
    }

    #[test]
    fn keypad_editing() {
        let mut b = typed(&["1", "2", ".", "5", ".", "±"]);
        assert_eq!(b, "−12.5");
        key(&mut b, "±");
        key(&mut b, "⌫");
        assert_eq!(b, "12.");
    }

    #[test]
    fn keypad_expressions() {
        assert_eq!(typed(&["1", ".", "5", "+", "0", ".", "2", "5"]), "1.5+0.25");
        assert_eq!(typed(&["(", "2", "−", ".", "5", ")", "÷", "2"]), "(2−0.5)÷2");
        // An operator after an operator replaces it; a minus after × is a sign.
        assert_eq!(typed(&["3", "+", "×", "2"]), "3×2");
        assert_eq!(typed(&["3", "×", "−", "2"]), "3×−2");
        assert_eq!(typed(&["3", "×", "−", "+", "2"]), "3+2");
        // Only a sign can start a number.
        assert_eq!(typed(&["×", "+", "−", "4"]), "−4");
        // ± flips the last number only.
        assert_eq!(typed(&["5", "−", "2", "±"]), "5−−2");
        assert_eq!(typed(&["5", "−", "2", "±", "±"]), "5−2");
        assert_eq!(typed(&["5", "×", "±", "2"]), "5×−2");
        // Keys that can't fit are ignored.
        assert_eq!(typed(&[")", "2", ")", "(", "1", ".", "2", ".", "3"]), "21.23");
        assert_eq!(typed(&["(", "1", ")", "2", "."]), "(1)");
        for (keys, want) in [(&["1", ".", "5", "+", ".", "2", "5"][..], 1.75), (&["(", "2", "−", ".", "5", ")", "÷", "2"][..], 0.75), (&["5", "−", "2", "±"][..], 7.0)] {
            assert_eq!(calc::eval(&typed(keys)), Ok(want));
        }
    }

    #[test]
    fn keypad_fresh_value() {
        let pad = |buf: &str| Pad { what: "gt-x".into(), buf: buf.into(), fresh: true, error: Some("old".into()) };
        // An operator works on the old value, a digit replaces it.
        let mut p = pad("12.5");
        for k in ["+", "1"] {
            p.key(k);
        }
        assert_eq!((p.buf.as_str(), p.error.is_none()), ("12.5+1", true));
        let mut p = pad("12.5");
        p.key("3");
        assert_eq!(p.buf, "3");
        let mut p = pad("12.5");
        p.key("(");
        assert_eq!(p.buf, "(");
        let mut p = pad("12.5");
        for k in ["q", "/", "2"] {
            p.key(k);
        }
        assert_eq!(p.buf, "12.5÷2");
        let mut p = pad("12.5");
        p.key("±");
        assert_eq!(p.buf, "−12.5");
    }

    #[test]
    fn keypad_preview() {
        let pad = |buf: &str| Pad { what: "gt-x".into(), buf: buf.into(), fresh: false, error: None };
        assert_eq!(preview(&pad("12.5")), (String::new(), false));
        assert_eq!(preview(&pad("1÷3")), ("= 0.3333".into(), false));
        assert_eq!(preview(&pad("1−3")), ("= −2".into(), false));
        assert_eq!(preview(&pad("1+")), (String::new(), false));
        let mut p = pad("1÷0");
        p.error = calc::eval(&p.buf).err();
        assert_eq!(preview(&p), ("Can't divide by zero".into(), true));
    }

    #[test]
    fn speed_limits_from_ini() {
        let mut m = Manual::from_config(&Config::default(), Settings::memory());
        assert!(m.apply("jog-speed", 500.0).is_err());
        assert!(m.apply("jog-speed", 25.0).is_ok());
        assert_eq!(m.speed, 25.0);
    }
}

/// The screen's axis number (0 = X) as a jog target.
fn jog_target(axis: i32) -> Option<JogTarget> {
    usize::try_from(axis).ok().and_then(machine::Axis::from_index).map(JogTarget::Axis)
}
