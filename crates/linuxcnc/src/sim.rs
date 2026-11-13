//! In-process stand-in for LinuxCNC, so screens run on the Mac and in tests.
//! It models the state machine and motion roughly, not the interpreter.

use crate::status::*;
use std::collections::VecDeque;
use std::time::Instant;

pub struct Sim {
    s: Status,
    serial: i32,
    last: Instant,
    jog: [f64; AXES],
    run_until: Option<i32>,
    messages: VecDeque<Message>,
    selected_tool: i32,
    /// G54..G59.3: X Y Z A offsets and XY rotation.
    wcs: [[f64; 5]; 9],
}

impl Sim {
    /// A 4-axis mill (XYZA) with a few offsets set so the DRO shows something.
    pub fn new() -> Self {
        let mut s = Status::default();
        s.joints = 4;
        s.traj_mode = 1;
        s.axis_mask = 0b1111;
        s.g5x_index = 1;
        s.feed_scale = 1.0;
        s.rapid_scale = 1.0;
        s.spindle_scale = 1.0;
        s.actual = [180.0, -80.0, -182.4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        s.commanded = s.actual;
        s.g5x_offset[0] = 120.0;
        s.g5x_offset[1] = -95.0;
        s.g5x_offset[2] = -212.4;
        s.tool_in_spindle = 23;
        s.active_gcodes = [0, 10, 170, 210, 400, 490, 540, 640, 800, 900, 911, 940, 970, 980, -1, -1, -1];
        s.active_mcodes = [0, 5, 9, 48, -1, -1, -1, -1, -1, -1];
        Self { s, serial: 0, last: Instant::now(), jog: [0.0; AXES], run_until: None, messages: VecDeque::new(), selected_tool: 0, wcs: [[120.0, -95.0, -212.4, 0.0, 0.0], [0.0; 5], [0.0; 5], [0.0; 5], [0.0; 5], [0.0; 5], [0.0; 5], [0.0; 5], [0.0; 5]] }
    }

    fn msg(&mut self, kind: MessageKind, text: impl Into<String>) {
        self.messages.push_back(Message { kind, text: text.into() });
    }

    fn require_on(&mut self, what: &str) -> bool {
        if self.s.task_state != TaskState::On {
            self.msg(MessageKind::Error, format!("Can't do {what} when machine is not on"));
            return false;
        }
        true
    }
}

impl Sim {
    /// Just enough G-code for the manual screens: M7/M9, G10 L20 P0, G0/G53 G0.
    fn mdi(&mut self, line: &str) {
        let up = line.to_ascii_uppercase();
        let words: Vec<(char, f64)> = up
            .split_whitespace()
            .filter_map(|w| {
                let mut c = w.chars();
                let l = c.next()?;
                c.as_str().parse().ok().map(|v| (l, v))
            })
            .collect();
        let has = |l: char, v: f64| words.iter().any(|&(a, b)| a == l && b == v);
        let val = |l: char| words.iter().find(|&&(a, _)| a == l).map(|&(_, v)| v);
        if let Some(t) = val('T') {
            self.selected_tool = t as i32;
        }
        if has('M', 61.0) {
            self.s.tool_in_spindle = val('Q').unwrap_or(0.0) as i32;
        }
        if has('M', 6.0) {
            // Wait for the GUI, as iocontrol's manual tool change does.
            self.s.sim_tool_change = Some(self.selected_tool);
            self.s.interp = Interp::Reading;
        }
        if has('M', 7.0) {
            self.s.mist = true;
        }
        if has('M', 9.0) {
            self.s.mist = false;
            self.s.flood = false;
        }
        let codes = [54.0, 55.0, 56.0, 57.0, 58.0, 59.0, 59.1, 59.2, 59.3];
        let active = (self.s.g5x_index - 1).clamp(0, 8) as usize;
        if has('G', 10.0) && (has('L', 20.0) || has('L', 2.0)) {
            let p = val('P').unwrap_or(0.0) as usize;
            let n = if p == 0 { active } else { (p - 1).min(8) };
            for (i, l) in ['X', 'Y', 'Z', 'A'].into_iter().enumerate() {
                if let Some(v) = val(l) {
                    self.wcs[n][i] = if has('L', 20.0) { self.s.actual[i] - self.s.tool_offset[i] - self.s.g92_offset[i] - v } else { v };
                }
            }
            if let Some(r) = val('R') {
                self.wcs[n][4] = r;
            }
        } else if has('G', 92.1) {
            self.s.g92_offset = [0.0; AXES];
        } else if let Some(n) = codes.iter().position(|c| has('G', *c)) {
            self.s.g5x_index = n as i32 + 1;
            let code = (codes[n] * 10.0).round() as i32;
            for g in self.s.active_gcodes.iter_mut().filter(|g| (540..=593).contains(*g)) {
                *g = code;
            }
        } else if has('G', 0.0) || has('G', 1.0) {
            let machine = has('G', 53.0);
            for (i, l) in ['X', 'Y', 'Z', 'A'].into_iter().enumerate() {
                if let Some(v) = val(l) {
                    self.s.actual[i] = if machine { v } else { v + self.s.g5x_offset[i] + self.s.tool_offset[i] };
                }
            }
        }
        let a = (self.s.g5x_index - 1).clamp(0, 8) as usize;
        for i in 0..4 {
            self.s.g5x_offset[i] = self.wcs[a][i];
        }
        self.s.rotation_xy = self.wcs[a][4];
        self.msg(MessageKind::Display, format!("(sim) MDI {line}"));
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine for Sim {
    fn poll(&mut self) -> Result<Status, String> {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        let mut moving = false;
        for i in 0..AXES {
            if self.jog[i] != 0.0 {
                self.s.actual[i] += self.jog[i] / 60.0 * dt;
                moving = true;
            }
        }
        self.s.current_vel = if moving { self.jog.iter().map(|v| v.abs()).fold(0.0, f64::max) / 60.0 } else { 0.0 };
        if let Some(end) = self.run_until {
            if self.s.interp == Interp::Reading {
                self.s.current_line += 1;
                self.s.motion_line = self.s.current_line;
                if self.s.current_line >= end {
                    self.s.interp = Interp::Idle;
                    self.run_until = None;
                }
            }
        }
        self.s.commanded = self.s.actual;
        self.s.echo_serial = self.serial;
        self.s.command_status = 1;
        Ok(self.s.clone())
    }

    fn send(&mut self, cmd: &Command) -> Result<i32, String> {
        self.serial += 1;
        match cmd {
            Command::SetState(st) => {
                let ok = match st {
                    TaskState::Estop | TaskState::EstopReset => true,
                    TaskState::On | TaskState::Off => self.s.task_state != TaskState::Estop,
                };
                if ok {
                    self.s.task_state = *st;
                    self.s.enabled = *st == TaskState::On;
                    if !self.s.enabled {
                        self.jog = [0.0; AXES];
                        self.s.interp = Interp::Idle;
                    }
                } else {
                    self.msg(MessageKind::Error, "Can't turn machine on while in E-stop");
                }
            }
            Command::SetMode(m) => {
                if self.s.busy() && *m != self.s.mode {
                    self.msg(MessageKind::Error, "Can't change mode while running");
                } else {
                    self.s.mode = *m;
                }
            }
            Command::Abort => {
                self.s.sim_tool_change = None;
                self.s.interp = Interp::Idle;
                self.run_until = None;
                self.jog = [0.0; AXES];
            }
            Command::Mdi(line) => {
                if self.require_on("MDI") {
                    self.mdi(line);
                }
            }
            Command::ProgramOpen(path) => self.s.file = path.clone(),
            Command::ProgramRun(line) => {
                if self.require_on("run") {
                    self.s.mode = Mode::Auto;
                    self.s.interp = Interp::Reading;
                    self.s.current_line = (*line).max(1);
                    self.run_until = Some(self.s.current_line + 200);
                }
            }
            Command::Pause => {
                if self.s.interp == Interp::Reading {
                    self.s.interp = Interp::Paused;
                    self.s.paused = true;
                }
            }
            Command::Resume => {
                if self.s.interp == Interp::Paused {
                    self.s.interp = Interp::Reading;
                    self.s.paused = false;
                }
            }
            Command::Step => {}
            Command::OptionalStop(on) => self.s.optional_stop = *on,
            Command::BlockDelete(on) => self.s.block_delete = *on,
            Command::Home(j) => {
                if self.require_on("home") {
                    for i in 0..self.s.joints {
                        if *j < 0 || *j as usize == i {
                            self.s.homed[i] = true;
                        }
                    }
                }
            }
            Command::Unhome(j) => {
                for i in 0..self.s.joints {
                    if *j < 0 || *j as usize == i {
                        self.s.homed[i] = false;
                    }
                }
            }
            Command::Teleop(on) => self.s.traj_mode = if *on { 3 } else { 1 },
            Command::JogCont { axis: index, vel } => {
                if self.require_on("jog") {
                    if let Some(v) = self.jog.get_mut(*index as usize) {
                        *v = *vel * 60.0;
                    }
                }
            }
            Command::JogIncr { axis: index, incr, .. } => {
                if self.require_on("jog") {
                    if let Some(p) = self.s.actual.get_mut(*index as usize) {
                        *p += *incr;
                    }
                }
            }
            Command::JogStop { axis: index } => {
                if let Some(v) = self.jog.get_mut(*index as usize) {
                    *v = 0.0;
                }
            }
            Command::FeedScale(s) => self.s.feed_scale = *s,
            Command::RapidScale(s) => self.s.rapid_scale = *s,
            Command::SpindleScale(s) => self.s.spindle_scale = *s,
            Command::SpindleOn(rpm) => {
                if self.require_on("spindle") {
                    self.s.spindle_speed = rpm.abs();
                    self.s.spindle_direction = rpm.signum() as i32;
                    self.s.spindle_enabled = true;
                }
            }
            Command::SpindleOff => {
                self.s.spindle_speed = 0.0;
                self.s.spindle_direction = 0;
                self.s.spindle_enabled = false;
            }
            Command::LoadToolTable | Command::PlanSynch => {}
            Command::SimToolChanged => {
                if let Some(t) = self.s.sim_tool_change.take() {
                    self.s.tool_in_spindle = t;
                    self.s.interp = Interp::Idle;
                    self.msg(MessageKind::Display, format!("(sim) T{t} loaded"));
                }
            }
        }
        Ok(self.serial)
    }

    fn next_message(&mut self) -> Option<Message> {
        self.messages.pop_front()
    }
}
