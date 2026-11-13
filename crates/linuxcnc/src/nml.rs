//! NML backend over cpp/lcnc_shim.

use crate::status::*;
use std::ffi::{c_char, c_int, CStr, CString};

const PATH: usize = 256;

/// Mirror of lcnc_status_t. Field order must match cpp/lcnc_shim.h; `open`
/// checks the size against the C side.
#[repr(C)]
struct Raw {
    task_mode: i32,
    task_state: i32,
    exec_state: i32,
    interp_state: i32,
    call_level: i32,
    motion_line: i32,
    current_line: i32,
    read_line: i32,
    optional_stop: i32,
    block_delete: i32,
    task_paused: i32,
    queued_mdi: i32,
    g5x_index: i32,
    program_units: i32,
    interp_errcode: i32,
    delay_left: f64,
    rotation_xy: f64,
    g5x_offset: [f64; AXES],
    g92_offset: [f64; AXES],
    tool_offset: [f64; AXES],
    active_gcodes: [i32; 17],
    active_mcodes: [i32; 10],
    active_settings: [f64; 5],
    file: [c_char; PATH],
    command: [c_char; PATH],
    ini_filename: [c_char; PATH],

    joints: i32,
    axis_mask: i32,
    traj_mode: i32,
    enabled: i32,
    inpos: i32,
    queue: i32,
    paused: i32,
    probe_tripped: i32,
    probing: i32,
    probeval: i32,
    motion_type: i32,
    feed_hold_enabled: i32,
    feed_override_enabled: i32,
    linear_units: f64,
    angular_units: f64,
    cycle_time: f64,
    feed_scale: f64,
    rapid_scale: f64,
    current_vel: f64,
    distance_to_go: f64,
    max_velocity: f64,
    position: [f64; AXES],
    actual_position: [f64; AXES],
    dtg: [f64; AXES],
    probed_position: [f64; AXES],

    homed: [u8; JOINTS],
    homing: [u8; JOINTS],
    fault: [u8; JOINTS],
    min_soft_limit: [u8; JOINTS],
    max_soft_limit: [u8; JOINTS],
    min_hard_limit: [u8; JOINTS],
    max_hard_limit: [u8; JOINTS],
    ferror: [f64; JOINTS],
    on_soft_limit: i32,

    spindle_speed: f64,
    spindle_scale: f64,
    spindle_direction: i32,
    spindle_enabled: i32,
    spindle_brake: i32,
    spindle_override_enabled: i32,

    estop: i32,
    tool_in_spindle: i32,
    pocket_prepped: i32,
    mist: i32,
    flood: i32,

    echo_serial: i32,
    command_status: i32,
}

#[repr(C)]
struct Lcnc {
    _p: [u8; 0],
}

extern "C" {
    fn lcnc_status_size() -> usize;
    fn lcnc_open(nml: *const c_char, err: *mut c_char, len: usize) -> *mut Lcnc;
    fn lcnc_close(c: *mut Lcnc);
    fn lcnc_poll(c: *mut Lcnc, out: *mut Raw) -> c_int;
    fn lcnc_next_error(c: *mut Lcnc, buf: *mut c_char, len: usize) -> c_int;
    fn lcnc_set_state(c: *mut Lcnc, s: c_int) -> c_int;
    fn lcnc_set_mode(c: *mut Lcnc, m: c_int) -> c_int;
    fn lcnc_abort(c: *mut Lcnc) -> c_int;
    fn lcnc_mdi(c: *mut Lcnc, line: *const c_char) -> c_int;
    fn lcnc_program_open(c: *mut Lcnc, path: *const c_char) -> c_int;
    fn lcnc_program_run(c: *mut Lcnc, line: c_int) -> c_int;
    fn lcnc_program_pause(c: *mut Lcnc) -> c_int;
    fn lcnc_program_resume(c: *mut Lcnc) -> c_int;
    fn lcnc_program_step(c: *mut Lcnc) -> c_int;
    fn lcnc_set_optional_stop(c: *mut Lcnc, on: c_int) -> c_int;
    fn lcnc_set_block_delete(c: *mut Lcnc, on: c_int) -> c_int;
    fn lcnc_home(c: *mut Lcnc, joint: c_int) -> c_int;
    fn lcnc_unhome(c: *mut Lcnc, joint: c_int) -> c_int;
    fn lcnc_teleop(c: *mut Lcnc, on: c_int) -> c_int;
    fn lcnc_jog_cont(c: *mut Lcnc, jm: c_int, i: c_int, vel: f64) -> c_int;
    fn lcnc_jog_incr(c: *mut Lcnc, jm: c_int, i: c_int, vel: f64, incr: f64) -> c_int;
    fn lcnc_jog_stop(c: *mut Lcnc, jm: c_int, i: c_int) -> c_int;
    fn lcnc_feed_scale(c: *mut Lcnc, s: f64) -> c_int;
    fn lcnc_rapid_scale(c: *mut Lcnc, s: f64) -> c_int;
    fn lcnc_spindle_scale(c: *mut Lcnc, s: f64) -> c_int;
    fn lcnc_spindle_on(c: *mut Lcnc, rpm: f64) -> c_int;
    fn lcnc_spindle_off(c: *mut Lcnc) -> c_int;
    fn lcnc_load_tool_table(c: *mut Lcnc) -> c_int;
    fn lcnc_task_plan_synch(c: *mut Lcnc) -> c_int;
}

pub struct Nml {
    c: *mut Lcnc,
    raw: Box<Raw>,
    /// Joint jogs until homed, axis jogs after (from the last poll).
    joint_mode: bool,
}

// The connection is only ever used from the thread that owns the Nml.
unsafe impl Send for Nml {}

fn cstr(buf: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

fn axes(a: &[f64; AXES]) -> [f64; AXES] {
    *a
}

fn flags(a: &[u8; JOINTS]) -> [bool; JOINTS] {
    a.map(|v| v != 0)
}

impl Nml {
    pub fn open(nml_file: Option<&str>) -> Result<Self, String> {
        let size = unsafe { lcnc_status_size() };
        if size != std::mem::size_of::<Raw>() {
            return Err(format!("lcnc_status_t is {size} bytes in C but {} in Rust", std::mem::size_of::<Raw>()));
        }
        let file = nml_file.map(|f| CString::new(f).unwrap());
        let mut err = [0 as c_char; 256];
        let c = unsafe {
            lcnc_open(file.as_ref().map_or(std::ptr::null(), |f| f.as_ptr()), err.as_mut_ptr(), err.len())
        };
        if c.is_null() {
            return Err(cstr(&err));
        }
        // Raw is plain data; all-zero is a valid value.
        let raw: Box<Raw> = unsafe { Box::new(std::mem::zeroed()) };
        Ok(Self { c, raw, joint_mode: true })
    }
}

impl Drop for Nml {
    fn drop(&mut self) {
        unsafe { lcnc_close(self.c) }
    }
}

impl Machine for Nml {
    fn poll(&mut self) -> Result<Status, String> {
        if unsafe { lcnc_poll(self.c, &mut *self.raw) } != 0 {
            return Err("LinuxCNC status channel is not available".into());
        }
        let r = &*self.raw;
        // Teleop (axis jogs) only exists once homed; before that jog joints.
        self.joint_mode = r.traj_mode != 3;
        Ok(Status {
            task_state: TaskState::from_raw(r.task_state),
            mode: Mode::from_raw(r.task_mode),
            interp: Interp::from_raw(r.interp_state),
            exec_state: r.exec_state,
            enabled: r.enabled != 0,
            traj_mode: r.traj_mode,
            joints: r.joints.max(0) as usize,
            axis_mask: r.axis_mask as u32,
            homed: flags(&r.homed),
            homing: flags(&r.homing),
            actual: axes(&r.actual_position),
            commanded: axes(&r.position),
            dtg: axes(&r.dtg),
            g5x_offset: axes(&r.g5x_offset),
            g92_offset: axes(&r.g92_offset),
            tool_offset: axes(&r.tool_offset),
            rotation_xy: r.rotation_xy,
            g5x_index: r.g5x_index,
            tool_in_spindle: r.tool_in_spindle,
            feed_scale: r.feed_scale,
            rapid_scale: r.rapid_scale,
            spindle_scale: r.spindle_scale,
            current_vel: r.current_vel,
            spindle_speed: r.spindle_speed,
            spindle_direction: r.spindle_direction,
            spindle_enabled: r.spindle_enabled != 0,
            mist: r.mist != 0,
            flood: r.flood != 0,
            active_gcodes: r.active_gcodes,
            active_mcodes: r.active_mcodes,
            active_settings: r.active_settings,
            file: cstr(&r.file),
            current_line: r.current_line,
            motion_line: r.motion_line,
            read_line: r.read_line,
            optional_stop: r.optional_stop != 0,
            block_delete: r.block_delete != 0,
            paused: r.paused != 0 || r.task_paused != 0,
            queued_mdi: r.queued_mdi,
            probe_tripped: r.probe_tripped != 0,
            probing: r.probing != 0,
            on_soft_limit: r.on_soft_limit != 0,
            echo_serial: r.echo_serial,
            command_status: r.command_status,
            sim_tool_change: None,
        })
    }

    fn send(&mut self, cmd: &Command) -> Result<i32, String> {
        let c = self.c;
        let rc = unsafe {
            match cmd {
                Command::SetState(s) => lcnc_set_state(c, *s as c_int),
                Command::SetMode(m) => lcnc_set_mode(c, *m as c_int),
                Command::Abort => lcnc_abort(c),
                Command::Mdi(line) => {
                    let l = CString::new(line.as_str()).map_err(|e| e.to_string())?;
                    lcnc_mdi(c, l.as_ptr())
                }
                Command::ProgramOpen(path) => {
                    let p = CString::new(path.as_str()).map_err(|e| e.to_string())?;
                    lcnc_program_open(c, p.as_ptr())
                }
                Command::ProgramRun(line) => lcnc_program_run(c, *line),
                Command::Pause => lcnc_program_pause(c),
                Command::Resume => lcnc_program_resume(c),
                Command::Step => lcnc_program_step(c),
                Command::OptionalStop(on) => lcnc_set_optional_stop(c, *on as c_int),
                Command::BlockDelete(on) => lcnc_set_block_delete(c, *on as c_int),
                Command::Home(j) => lcnc_home(c, *j),
                Command::Unhome(j) => lcnc_unhome(c, *j),
                Command::Teleop(on) => lcnc_teleop(c, *on as c_int),
                // The worker resolves joint mode; here teleop means axis jogs.
                Command::JogCont { axis, vel } => lcnc_jog_cont(c, self.joint_mode as c_int, *axis, *vel),
                Command::JogIncr { axis, vel, incr } => {
                    lcnc_jog_incr(c, self.joint_mode as c_int, *axis, *vel, *incr)
                }
                Command::JogStop { axis } => lcnc_jog_stop(c, self.joint_mode as c_int, *axis),
                Command::FeedScale(s) => lcnc_feed_scale(c, *s),
                Command::RapidScale(s) => lcnc_rapid_scale(c, *s),
                Command::SpindleScale(s) => lcnc_spindle_scale(c, *s),
                Command::SpindleOn(rpm) => lcnc_spindle_on(c, *rpm),
                Command::SpindleOff => lcnc_spindle_off(c),
                Command::LoadToolTable => lcnc_load_tool_table(c),
                Command::PlanSynch => lcnc_task_plan_synch(c),
                Command::SimToolChanged => 0,
            }
        };
        if rc < 0 {
            return Err(format!("failed to send {cmd:?}"));
        }
        Ok(rc)
    }

    fn next_message(&mut self) -> Option<Message> {
        let mut buf = [0 as c_char; 256];
        let kind = match unsafe { lcnc_next_error(self.c, buf.as_mut_ptr(), buf.len()) } {
            1 => MessageKind::Error,
            2 => MessageKind::Text,
            3 => MessageKind::Display,
            _ => return None,
        };
        Some(Message { kind, text: cstr(&buf) })
    }
}
