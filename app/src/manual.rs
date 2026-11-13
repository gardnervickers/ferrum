//! Manual control: jog, spindle, mist, work zero, power feed, the keypad and the
//! message log. Everything here is locked in the UI while a program runs or
//! is paused (`Live.manual-locked`); LinuxCNC enforces its own mode rules.

use crate::config::Config;
use crate::powerfeed::{self, Mode, PowerFeed};
use crate::settings::Settings;
use crate::{Actions, GotoRow, Live, MainWindow, MsgRow};
use linuxcnc::{Command, Handle, Interp, Message, MessageKind, Status};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const A_AXIS: i32 = 3;
const LOG: usize = 300;

struct Range {
    min: f64,
    max: f64,
}

pub struct Manual {
    /// "CONT" or an increment in machine units.
    step: String,
    speed: f64,
    speed_a: f64,
    rpm: f64,
    linear: Range,
    angular: Range,
    keypad: Option<(String, String)>,
    status: Option<Status>,
    pf: PowerFeed,
    settings: Settings,
    /// Last power feed / GO TO message, and whether it is an error.
    pf_note: Option<(String, bool)>,
    log: Rc<VecModel<MsgRow>>,
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Manual {
    fn from_config(cfg: &Config, settings: Settings) -> Self {
        let f = |k: &str, d: f64| cfg.get_f64("DISPLAY", k).unwrap_or(d);
        Self {
            step: "CONT".into(),
            speed: settings.f64("jog.speed", f("DEFAULT_LINEAR_VELOCITY", 50.0)),
            speed_a: settings.f64("jog.speed-a", f("DEFAULT_ANGULAR_VELOCITY", 180.0)),
            rpm: settings.f64("powerfeed.spindle-rpm", 300.0),
            pf: PowerFeed::new(cfg, &settings),
            settings,
            pf_note: None,
            linear: Range { min: f("MIN_LINEAR_VELOCITY", 0.01), max: f("MAX_LINEAR_VELOCITY", 100.0) },
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
        live.set_pf_axis_enabled(ModelRc::new(VecModel::from((0..3).map(|a| pf.axis_enabled(a)).collect::<Vec<_>>())));
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
        live.set_gotos(ModelRc::new(VecModel::from(rows)));
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
        if let Some((what, buf)) = &self.keypad {
            let l = KEYPAD.with(|t| t.borrow().iter().find_map(|t| t.label(what))).unwrap_or_else(|| label(what).to_string());
            live.set_keypad_label(l.into());
            live.set_keypad_value(buf.clone().into());
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
            "jog-speed" => self.speed = check(v, &self.linear, "mm/s")?,
            "jog-speed-a" => self.speed_a = check(v, &self.angular, "°/s")?,
            "spindle-rpm" => {
                if v <= 0.0 {
                    return Err("RPM must be above 0".into());
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

fn label(what: &str) -> &'static str {
    match what {
        "jog-speed" => "JOG SPEED MM/S",
        "jog-speed-a" => "A SPEED °/S",
        "spindle-rpm" => "SPINDLE RPM",
        "pf-feed" | "gt-feed" => "FEED MM/MIN",
        "pf-dist" => "DISTANCE MM",
        "gt-x" => "TARGET X (WORK)",
        "gt-y" => "TARGET Y (WORK)",
        _ => "VALUE",
    }
}

fn key(buf: &mut String, k: &str) {
    match k {
        "⌫" => {
            buf.pop();
        }
        "CLR" => buf.clear(),
        "±" => {
            if let Some(rest) = buf.strip_prefix('-') {
                *buf = rest.to_string();
            } else {
                buf.insert(0, '-');
            }
        }
        "." if buf.contains('.') => {}
        _ if buf.len() < 10 => buf.push_str(k),
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
    live.set_has_a(has_a);
    let steps: Vec<SharedString> = cfg
        .get("DISPLAY", "INCREMENTS")
        .map(|v| {
            v.split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| if s.eq_ignore_ascii_case("JOG") { "CONT".into() } else { s.into() })
                .collect()
        })
        .unwrap_or_else(|| ["CONT", "0.01", "0.1", "1", "10"].map(SharedString::from).to_vec());
    live.set_jog_steps(ModelRc::new(VecModel::from(steps)));

    let (mc, mm) = (machine.clone(), manual.clone());
    actions.on_jog_start(move |axis, dir| {
        let m = mm.borrow();
        if m.status.as_ref().is_some_and(locked) {
            return;
        }
        let speed = if axis == A_AXIS { m.speed_a } else { m.speed };
        match m.step.parse::<f64>() {
            Ok(incr) => mc.send(Command::JogIncr { axis, vel: speed, incr: incr * dir as f64 }),
            Err(_) => mc.send(Command::JogCont { axis, vel: speed * dir as f64 }),
        }
    });
    let mc = machine.clone();
    actions.on_jog_stop(move |axis| mc.send(Command::JogStop { axis }));

    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_jog_step_set(move |step| {
        let mut m = mm.borrow_mut();
        m.step = step.into();
        if let Some(ui) = weak.upgrade() {
            m.show(&ui.global::<Live>());
        }
    });

    // A has no home switch; homing joint 3 with no search velocity latches
    // the current position as its home.
    let mc = machine.clone();
    actions.on_set_a_reference(move || mc.send(Command::Home(A_AXIS)));

    let (mc, mm) = (machine.clone(), manual.clone());
    actions.on_spindle(move |dir| {
        let rpm = mm.borrow().rpm;
        mc.send(if dir == 0 { Command::SpindleOff } else { Command::SpindleOn(rpm * dir as f64) });
    });

    let (mc, mm) = (machine.clone(), manual.clone());
    actions.on_mist_toggle(move || {
        let on = mm.borrow().status.as_ref().is_some_and(|s| s.mist);
        mc.send(Command::Mdi(if on { "M9" } else { "M7" }.into()));
    });

    let mc = machine.clone();
    actions.on_zero_xyz(move || mc.send(Command::Mdi("G10 L20 P0 X0 Y0 Z0".into())));
    // Both run in the active WCS through o<mymill_powerfeed>, which restores
    // modal state afterwards.
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
        let cmd = m.ready(&ui.global::<Live>()).and_then(|_| m.pf.retract_cmd(m.status.as_ref().unwrap(), 10.0));
        match cmd {
            Ok(c) => mc.send(Command::Mdi(c)),
            Err(e) => note(&ui, e, true),
        }
    });

    wire_powerfeed(ui, machine, &manual);

    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_edit(move |what| {
        let mut m = mm.borrow_mut();
        let cur = fmt(m.current(&what));
        m.keypad = Some((what.into(), cur));
        if let Some(ui) = weak.upgrade() {
            m.show(&ui.global::<Live>());
        }
    });
    let (mm, weak) = (manual.clone(), ui.as_weak());
    actions.on_keypad_key(move |k| {
        let mut m = mm.borrow_mut();
        if let Some((_, buf)) = m.keypad.as_mut() {
            key(buf, &k);
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
            if let Some((what, buf)) = m.keypad.clone() {
                let r = buf.parse::<f64>().map_err(|_| format!("Not a number: {buf}"));
                if let Err(e) = r.and_then(|v| m.apply(&what, v)) {
                    // Keep the keypad open so the entry can be corrected.
                    note(&ui, e, true);
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

    #[test]
    fn keypad_editing() {
        let mut b = String::new();
        for k in ["1", "2", ".", "5", ".", "±"] {
            key(&mut b, k);
        }
        assert_eq!(b, "-12.5");
        key(&mut b, "±");
        key(&mut b, "⌫");
        assert_eq!(b, "12.");
    }

    #[test]
    fn speed_limits_from_ini() {
        let mut m = Manual::from_config(&Config::default(), Settings::memory());
        assert!(m.apply("jog-speed", 500.0).is_err());
        assert!(m.apply("jog-speed", 25.0).is_ok());
        assert_eq!(m.speed, 25.0);
    }
}
