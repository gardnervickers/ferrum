//! Manual control: jog, spindle, mist, work zero, MDI, the keypad and the
//! message log. Everything here is locked in the UI while a program runs or
//! is paused (`Live.manual-locked`); LinuxCNC enforces its own mode rules.

use crate::config::Config;
use crate::{Actions, Live, MainWindow, MsgRow};
use linuxcnc::{Command, Handle, Interp, Message, MessageKind, Status};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

const A_AXIS: i32 = 3;
const HISTORY: usize = 50;
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
    history: Rc<VecModel<SharedString>>,
    log: Rc<VecModel<MsgRow>>,
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Manual {
    fn from_config(cfg: &Config) -> Self {
        let f = |k: &str, d: f64| cfg.get_f64("DISPLAY", k).unwrap_or(d);
        Self {
            step: "CONT".into(),
            speed: f("DEFAULT_LINEAR_VELOCITY", 50.0),
            speed_a: f("DEFAULT_ANGULAR_VELOCITY", 180.0),
            rpm: 300.0,
            linear: Range { min: f("MIN_LINEAR_VELOCITY", 0.01), max: f("MAX_LINEAR_VELOCITY", 100.0) },
            angular: Range { min: f("MIN_ANGULAR_VELOCITY", 0.01), max: f("MAX_ANGULAR_VELOCITY", 180.0) },
            keypad: None,
            status: None,
            history: Rc::new(VecModel::default()),
            log: Rc::new(VecModel::default()),
        }
    }

    fn show(&self, live: &Live) {
        live.set_jog_step(self.step.clone().into());
        live.set_jog_speed(fmt(self.speed).into());
        live.set_jog_speed_a(fmt(self.speed_a).into());
        live.set_spindle_rpm(fmt(self.rpm).into());
        live.set_keypad_open(self.keypad.is_some());
        if let Some((what, buf)) = &self.keypad {
            live.set_keypad_label(label(what).into());
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
            _ => {}
        }
        Ok(())
    }

    fn current(&self, what: &str) -> f64 {
        match what {
            "jog-speed" => self.speed,
            "jog-speed-a" => self.speed_a,
            "spindle-rpm" => self.rpm,
            _ => 0.0,
        }
    }
}

fn label(what: &str) -> &'static str {
    match what {
        "jog-speed" => "JOG SPEED MM/S",
        "jog-speed-a" => "A SPEED °/S",
        "spindle-rpm" => "SPINDLE RPM",
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
    manual.borrow_mut().status = Some(s.clone());
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

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, has_a: bool) -> Rc<RefCell<Manual>> {
    let manual = Rc::new(RefCell::new(Manual::from_config(cfg)));
    let actions = ui.global::<Actions>();
    let live = ui.global::<Live>();
    {
        let m = manual.borrow();
        live.set_mdi_history(ModelRc::from(m.history.clone()));
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
    let mc = machine.clone();
    actions.on_z_to_work_zero(move || mc.send(Command::Mdi("G90 G0 Z0".into())));
    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_retract_xy_to_zero(move || {
        let Some(s) = mm.borrow().status.clone() else { return };
        // Absolute moves, so the result does not depend on G90/G91 (G90 stays set).
        let z = s.work_position()[2] + 10.0;
        if let Some(ui) = weak.upgrade() {
            note(&ui, format!("Retract to work Z{z:.3}, then XY to work 0"), false);
        }
        mc.send(Command::Mdi(format!("G90 G0 Z{z:.4}")));
        mc.send(Command::Mdi("G0 X0 Y0".into()));
    });

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

    let (mc, mm, weak) = (machine.clone(), manual.clone(), ui.as_weak());
    actions.on_mdi_send(move |text| {
        let line = text.trim().to_string();
        if line.is_empty() {
            return;
        }
        let hist = mm.borrow().history.clone();
        if let Some(i) = hist.iter().position(|h| h.as_str() == line) {
            hist.remove(i);
        }
        hist.insert(0, line.clone().into());
        while hist.row_count() > HISTORY {
            hist.remove(HISTORY);
        }
        mc.send(Command::Mdi(line));
        if let Some(ui) = weak.upgrade() {
            ui.global::<Live>().set_mdi_text("".into());
        }
    });

    let mm = manual.clone();
    actions.on_clear_messages(move || {
        let log = mm.borrow().log.clone();
        log.set_vec(Vec::new());
    });

    manual
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
        let mut m = Manual::from_config(&Config::default());
        assert!(m.apply("jog-speed", 500.0).is_err());
        assert!(m.apply("jog-speed", 25.0).is_ok());
        assert_eq!(m.speed, 25.0);
    }
}
