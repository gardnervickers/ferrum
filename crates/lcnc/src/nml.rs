//! The NML backend: `Connection` over `cpp/lcnc_shim`.

use crate::command::*;
use crate::error::{Error, Result};
use crate::status::Status;
use crate::types::*;
use std::ffi::{c_char, c_int, CStr, CString};
use std::path::Path;
use std::time::{Duration, Instant};

const PATH: usize = 256;

/// Mirror of lcnc_status_t. Field order must match cpp/lcnc_shim.h;
/// `Connection::open` checks the size against the C side.
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
    active_gcodes: [i32; GCODE_SLOTS],
    active_mcodes: [i32; MCODE_SLOTS],
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
    fn lcnc_override_limits(c: *mut Lcnc) -> c_int;
    fn lcnc_teleop(c: *mut Lcnc, on: c_int) -> c_int;
    fn lcnc_jog_cont(c: *mut Lcnc, jm: c_int, i: c_int, vel: f64) -> c_int;
    fn lcnc_jog_incr(c: *mut Lcnc, jm: c_int, i: c_int, vel: f64, incr: f64) -> c_int;
    fn lcnc_jog_stop(c: *mut Lcnc, jm: c_int, i: c_int) -> c_int;
    fn lcnc_feed_scale(c: *mut Lcnc, s: f64) -> c_int;
    fn lcnc_rapid_scale(c: *mut Lcnc, s: f64) -> c_int;
    fn lcnc_spindle_scale(c: *mut Lcnc, s: f64) -> c_int;
    fn lcnc_spindle_on(c: *mut Lcnc, rpm: f64) -> c_int;
    fn lcnc_spindle_off(c: *mut Lcnc) -> c_int;
    fn lcnc_coolant(c: *mut Lcnc, flood: c_int, on: c_int) -> c_int;
    fn lcnc_load_tool_table(c: *mut Lcnc) -> c_int;
    fn lcnc_task_plan_synch(c: *mut Lcnc) -> c_int;
    fn lcnc_spindle_brake(c: *mut Lcnc, engage: c_int) -> c_int;
    fn lcnc_max_velocity(c: *mut Lcnc, vel: f64) -> c_int;
    fn lcnc_feed_override_enable(c: *mut Lcnc, on: c_int) -> c_int;
    fn lcnc_spindle_override_enable(c: *mut Lcnc, on: c_int) -> c_int;
    fn lcnc_feed_hold_enable(c: *mut Lcnc, on: c_int) -> c_int;
}

/// A connection to a running LinuxCNC over its NML channels: status,
/// commands and operator messages. This is what LinuxCNC's Python
/// `linuxcnc.stat`, `linuxcnc.command` and `linuxcnc.error_channel` do,
/// in one object.
///
/// It is `Send` but not `Sync`: use it from one thread (typically a worker
/// that polls every few tens of milliseconds and runs commands from a
/// queue).
pub struct Connection {
    c: *mut Lcnc,
    raw: Box<Raw>,
    status: Status,
}

// NML handles may move between threads; they are never shared.
unsafe impl Send for Connection {}

impl Connection {
    /// Connect to a running LinuxCNC. `nml_file` defaults to the installed
    /// `linuxcnc.nml`.
    pub fn open(nml_file: Option<&Path>) -> Result<Self> {
        let size = unsafe { lcnc_status_size() };
        if size != std::mem::size_of::<Raw>() {
            return Err(Error::AbiMismatch { c: size, rust: std::mem::size_of::<Raw>() });
        }
        let file = match nml_file {
            Some(f) => Some(path_cstring(f)?),
            None => None,
        };
        let mut err = [0 as c_char; 256];
        let c = unsafe { lcnc_open(file.as_ref().map_or(std::ptr::null(), |f| f.as_ptr()), err.as_mut_ptr(), err.len()) };
        if c.is_null() {
            return Err(Error::Connect(unsafe { CStr::from_ptr(err.as_ptr()) }.to_string_lossy().into_owned()));
        }
        // Raw is plain data; all-zero is a valid value.
        let raw: Box<Raw> = unsafe { Box::new(std::mem::zeroed()) };
        Ok(Self { c, raw, status: Status::default() })
    }

    /// Read the newest status. Cheap when nothing changed: the snapshot is
    /// updated in place and strings are only reallocated when they change.
    pub fn poll(&mut self) -> Result<&Status> {
        match unsafe { lcnc_poll(self.c, &mut *self.raw) } {
            1 => decode(&self.raw, &mut self.status),
            0 => {}
            _ => return Err(Error::NoStatus),
        }
        Ok(&self.status)
    }

    /// The status from the last [`poll`](Self::poll).
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Send a command. Returns once task has taken it off the channel (or
    /// after a second), not when it is done; see [`wait_finished`](Self::wait_finished).
    pub fn send(&mut self, cmd: &Command) -> Result<Serial> {
        let c = self.c;
        let rc = unsafe {
            match cmd {
                Command::SetState(s) => lcnc_set_state(c, s.raw()),
                Command::SetMode(m) => lcnc_set_mode(c, m.raw()),
                Command::Abort => lcnc_abort(c),
                Command::Mdi(line) => {
                    let l = CString::new(line.as_str()).map_err(|_| Error::Nul)?;
                    lcnc_mdi(c, l.as_ptr())
                }
                Command::ProgramOpen(path) => {
                    let p = path_cstring(path)?;
                    lcnc_program_open(c, p.as_ptr())
                }
                Command::ProgramRun(line) => lcnc_program_run(c, (*line).min(i32::MAX as u32) as c_int),
                Command::Pause => lcnc_program_pause(c),
                Command::Resume => lcnc_program_resume(c),
                Command::Step => lcnc_program_step(c),
                Command::OptionalStop(on) => lcnc_set_optional_stop(c, *on as c_int),
                Command::BlockDelete(on) => lcnc_set_block_delete(c, *on as c_int),
                Command::Home(j) => lcnc_home(c, j.raw()),
                Command::Unhome(j) => lcnc_unhome(c, j.raw()),
                Command::OverrideLimits => lcnc_override_limits(c),
                Command::Teleop(on) => lcnc_teleop(c, *on as c_int),
                Command::JogCont { target, vel } => {
                    let (jm, i) = jog(*target);
                    lcnc_jog_cont(c, jm, i, *vel)
                }
                Command::JogIncr { target, vel, incr } => {
                    let (jm, i) = jog(*target);
                    lcnc_jog_incr(c, jm, i, *vel, *incr)
                }
                Command::JogStop { target } => {
                    let (jm, i) = jog(*target);
                    lcnc_jog_stop(c, jm, i)
                }
                Command::FeedScale(s) => lcnc_feed_scale(c, *s),
                Command::RapidScale(s) => lcnc_rapid_scale(c, *s),
                Command::SpindleScale(s) => lcnc_spindle_scale(c, *s),
                Command::SpindleOn { rpm, rotation } => {
                    let rpm = rpm.abs();
                    lcnc_spindle_on(c, if *rotation == Rotation::Reverse { -rpm } else { rpm })
                }
                Command::SpindleOff => lcnc_spindle_off(c),
                Command::SpindleBrake(on) => lcnc_spindle_brake(c, *on as c_int),
                Command::Mist(on) => lcnc_coolant(c, 0, *on as c_int),
                Command::Flood(on) => lcnc_coolant(c, 1, *on as c_int),
                Command::MaxVelocity(v) => lcnc_max_velocity(c, *v),
                Command::FeedOverrideEnable(on) => lcnc_feed_override_enable(c, *on as c_int),
                Command::SpindleOverrideEnable(on) => lcnc_spindle_override_enable(c, *on as c_int),
                Command::FeedHoldEnable(on) => lcnc_feed_hold_enable(c, *on as c_int),
                Command::LoadToolTable => lcnc_load_tool_table(c),
                Command::PlanSynch => lcnc_task_plan_synch(c),
            }
        };
        if rc < 0 {
            return Err(Error::Send(cmd.name()));
        }
        Ok(Serial(rc))
    }

    /// Wait until task has finished command `serial`, polling every
    /// millisecond. `Err(Failed)` when task reports it failed.
    pub fn wait_finished(&mut self, serial: Serial, timeout: Duration) -> Result<()> {
        let end = Instant::now() + timeout;
        loop {
            let s = self.poll()?;
            if s.finished(serial) {
                return if s.command_status == RcsStatus::Error { Err(Error::Failed) } else { Ok(()) };
            }
            if Instant::now() >= end {
                return Err(Error::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The next operator message, if one is waiting.
    pub fn next_message(&mut self) -> Option<Message> {
        let mut buf = [0 as c_char; 256];
        let kind = match unsafe { lcnc_next_error(self.c, buf.as_mut_ptr(), buf.len()) } {
            1 => MessageKind::Error,
            2 => MessageKind::Text,
            3 => MessageKind::Display,
            _ => return None,
        };
        Some(Message { kind, text: unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned() })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe { lcnc_close(self.c) }
    }
}

fn path_cstring(p: &Path) -> Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(p.as_os_str().as_bytes()).map_err(|_| Error::Nul)
}

fn jog(t: JogTarget) -> (c_int, c_int) {
    match t {
        JogTarget::Axis(a) => (0, a as c_int),
        JogTarget::Joint(j) => (1, j as c_int),
    }
}

/// Copy a NUL-terminated C buffer into `dst`, reusing its allocation and
/// skipping the copy when the text is unchanged.
fn set_str(dst: &mut String, src: &[c_char]) {
    let bytes = unsafe { CStr::from_ptr(src.as_ptr()) }.to_bytes();
    if dst.as_bytes() != bytes {
        dst.clear();
        dst.push_str(&String::from_utf8_lossy(bytes));
    }
}

fn flags(a: &[u8; JOINTS]) -> [bool; JOINTS] {
    a.map(|v| v != 0)
}

fn decode(r: &Raw, s: &mut Status) {
    s.task_state = TaskState::from_raw(r.task_state);
    s.mode = TaskMode::from_raw(r.task_mode);
    s.interp = InterpState::from_raw(r.interp_state);
    s.exec_state = ExecState::from_raw(r.exec_state);
    s.call_level = r.call_level;
    s.motion_line = r.motion_line;
    s.current_line = r.current_line;
    s.read_line = r.read_line;
    s.optional_stop = r.optional_stop != 0;
    s.block_delete = r.block_delete != 0;
    s.paused = r.paused != 0 || r.task_paused != 0;
    s.queued_mdi = r.queued_mdi;
    s.wcs = Wcs::from_raw(r.g5x_index);
    s.program_units = ProgramUnits::from_raw(r.program_units);
    s.interp_errcode = r.interp_errcode;
    s.delay_left = r.delay_left;
    s.rotation_xy = r.rotation_xy;
    s.g5x_offset = Pose(r.g5x_offset);
    s.g92_offset = Pose(r.g92_offset);
    s.tool_offset = Pose(r.tool_offset);
    s.active_gcodes = ActiveGCodes(r.active_gcodes);
    s.active_mcodes = ActiveMCodes(r.active_mcodes);
    s.active_settings = ActiveSettings(r.active_settings);
    set_str(&mut s.file, &r.file);
    set_str(&mut s.command, &r.command);
    set_str(&mut s.ini_filename, &r.ini_filename);

    s.joints = r.joints.max(0) as usize;
    s.axis_mask = AxisMask(r.axis_mask as u32);
    s.traj_mode = TrajMode::from_raw(r.traj_mode);
    s.enabled = r.enabled != 0;
    s.inpos = r.inpos != 0;
    s.queue = r.queue;
    s.probe_tripped = r.probe_tripped != 0;
    s.probing = r.probing != 0;
    s.probe_val = r.probeval != 0;
    s.motion_type = MotionType::from_raw(r.motion_type);
    s.feed_hold_enabled = r.feed_hold_enabled != 0;
    s.feed_override_enabled = r.feed_override_enabled != 0;
    s.linear_units = r.linear_units;
    s.angular_units = r.angular_units;
    s.cycle_time = r.cycle_time;
    s.feed_scale = r.feed_scale;
    s.rapid_scale = r.rapid_scale;
    s.current_vel = r.current_vel;
    s.distance_to_go = r.distance_to_go;
    s.max_velocity = r.max_velocity;
    s.commanded = Pose(r.position);
    s.actual = Pose(r.actual_position);
    s.dtg = Pose(r.dtg);
    s.probed_position = Pose(r.probed_position);

    s.homed = flags(&r.homed);
    s.homing = flags(&r.homing);
    s.fault = flags(&r.fault);
    s.min_soft_limit = flags(&r.min_soft_limit);
    s.max_soft_limit = flags(&r.max_soft_limit);
    s.min_hard_limit = flags(&r.min_hard_limit);
    s.max_hard_limit = flags(&r.max_hard_limit);
    s.ferror = r.ferror;
    s.on_soft_limit = r.on_soft_limit != 0;

    s.spindle_speed = r.spindle_speed;
    s.spindle_scale = r.spindle_scale;
    s.spindle_direction = SpindleDirection::from_raw(r.spindle_direction);
    s.spindle_enabled = r.spindle_enabled != 0;
    s.spindle_brake = r.spindle_brake != 0;
    s.spindle_override_enabled = r.spindle_override_enabled != 0;

    s.estop = r.estop != 0;
    s.tool_in_spindle = r.tool_in_spindle;
    s.pocket_prepped = r.pocket_prepped;
    s.mist = r.mist != 0;
    s.flood = r.flood != 0;

    s.echo_serial = Serial(r.echo_serial);
    s.command_status = RcsStatus::from_raw(r.command_status);
}
