//! A typed snapshot of LinuxCNC's status channel (`EMC_STAT`).

use crate::types::*;

/// One snapshot of the status channel, what LinuxCNC's Python `stat`
/// object reports, with typed values.
///
/// Positions are machine coordinates in the machine's linear units, angles
/// in degrees. Spindle values are for spindle 0. Joint arrays are
/// [`JOINTS`] long; only the first [`Status::joints`] entries mean anything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    // Task.
    pub task_state: TaskState,
    pub mode: TaskMode,
    pub interp: InterpState,
    pub exec_state: ExecState,
    /// Subroutine nesting depth of the interpreter.
    pub call_level: i32,
    /// The line motion is executing.
    pub motion_line: i32,
    /// The line the interpreter is executing.
    pub current_line: i32,
    /// The line the interpreter has read up to (read-ahead).
    pub read_line: i32,
    pub optional_stop: bool,
    pub block_delete: bool,
    /// Task or motion is paused (feed hold, M0/M1, pause).
    pub paused: bool,
    /// MDI commands waiting behind the running one.
    pub queued_mdi: i32,
    pub wcs: Wcs,
    pub program_units: ProgramUnits,
    pub interp_errcode: i32,
    /// Seconds left in a G4 dwell.
    pub delay_left: f64,
    /// G10 L2 R, degrees.
    pub rotation_xy: f64,
    pub g5x_offset: Pose,
    pub g92_offset: Pose,
    pub tool_offset: Pose,
    pub active_gcodes: ActiveGCodes,
    pub active_mcodes: ActiveMCodes,
    pub active_settings: ActiveSettings,
    /// The interpreter's current file: the program, or a subroutine file
    /// while one is being read.
    pub file: String,
    /// The MDI command being executed.
    pub command: String,
    pub ini_filename: String,

    // Trajectory.
    pub joints: usize,
    pub axis_mask: AxisMask,
    pub traj_mode: TrajMode,
    /// Motion enabled (machine on).
    pub enabled: bool,
    pub inpos: bool,
    /// Motions in the queue.
    pub queue: i32,
    pub probe_tripped: bool,
    pub probing: bool,
    pub probe_val: bool,
    pub motion_type: MotionType,
    pub feed_hold_enabled: bool,
    pub feed_override_enabled: bool,
    pub linear_units: f64,
    pub angular_units: f64,
    pub cycle_time: f64,
    /// Feed override, 1.0 = 100 %.
    pub feed_scale: f64,
    /// Rapid override, 1.0 = 100 %.
    pub rapid_scale: f64,
    /// Tool tip speed, units per second.
    pub current_vel: f64,
    pub distance_to_go: f64,
    pub max_velocity: f64,
    /// Commanded position.
    pub commanded: Pose,
    /// Feedback position.
    pub actual: Pose,
    pub dtg: Pose,
    pub probed_position: Pose,

    // Joints.
    pub homed: [bool; JOINTS],
    pub homing: [bool; JOINTS],
    pub fault: [bool; JOINTS],
    pub min_soft_limit: [bool; JOINTS],
    pub max_soft_limit: [bool; JOINTS],
    pub min_hard_limit: [bool; JOINTS],
    pub max_hard_limit: [bool; JOINTS],
    pub ferror: [f64; JOINTS],
    pub on_soft_limit: bool,

    // Spindle 0.
    /// Commanded speed, rpm.
    pub spindle_speed: f64,
    /// Spindle override, 1.0 = 100 %.
    pub spindle_scale: f64,
    pub spindle_direction: SpindleDirection,
    pub spindle_enabled: bool,
    pub spindle_brake: bool,
    pub spindle_override_enabled: bool,

    // IO.
    pub estop: bool,
    /// Tool number in the spindle, 0 for none.
    pub tool_in_spindle: i32,
    pub pocket_prepped: i32,
    pub mist: bool,
    pub flood: bool,

    /// Serial of the last command task received.
    pub echo_serial: Serial,
    /// How that command is getting on.
    pub command_status: RcsStatus,
}

impl Status {
    pub fn estop(&self) -> bool {
        self.task_state == TaskState::Estop
    }

    pub fn power_on(&self) -> bool {
        self.task_state == TaskState::On
    }

    pub fn has_axis(&self, axis: Axis) -> bool {
        self.axis_mask.has(axis)
    }

    /// Every joint is homed (false when no joints are reported).
    pub fn all_homed(&self) -> bool {
        self.joints > 0 && self.homed[..self.joints.min(JOINTS)].iter().all(|h| *h)
    }

    /// A program, MDI command or subroutine is running or paused.
    pub fn busy(&self) -> bool {
        self.interp != InterpState::Idle
    }

    /// Task has taken command `serial` and finished with it (done or failed).
    pub fn finished(&self, serial: Serial) -> bool {
        self.echo_serial >= serial && self.command_status != RcsStatus::Exec
    }

    /// The displayed (work) position, the way LinuxCNC's own GUIs compute
    /// it: actual minus tool offset and G5x, rotated by -R, minus G92.
    pub fn work_position(&self) -> Pose {
        let mut p = [0.0; AXES];
        for (i, v) in p.iter_mut().enumerate() {
            *v = self.actual[i] - self.tool_offset[i] - self.g5x_offset[i];
        }
        if self.rotation_xy != 0.0 {
            let t = (-self.rotation_xy).to_radians();
            let (x, y) = (p[0], p[1]);
            p[0] = x * t.cos() - y * t.sin();
            p[1] = x * t.sin() + y * t.cos();
        }
        for (i, v) in p.iter_mut().enumerate() {
            *v -= self.g92_offset[i];
        }
        Pose(p)
    }

    /// Active modal codes as "G1 G17 ... M5 M9", sorted. Lathe G7/G8, G80
    /// and the stop codes (M0 M1 M2 M30 M60) are left out.
    pub fn modal_string(&self) -> String {
        let mut g: Vec<GCode> = self.active_gcodes.iter().filter(|g| g.0 != 70 && g.0 != 80).collect();
        g.sort_unstable();
        g.dedup();
        let mut m: Vec<MCode> = self.active_mcodes.iter().filter(|m| ![0, 1, 2, 30, 60].contains(&m.0)).collect();
        m.sort_unstable();
        m.dedup();
        g.iter().map(ToString::to_string).chain(m.iter().map(ToString::to_string)).collect::<Vec<_>>().join(" ")
    }

    /// Tool tip feed in units per minute.
    pub fn feed_per_min(&self) -> f64 {
        self.current_vel * 60.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_string_formats_decimal_codes() {
        let mut s = Status::default();
        s.active_gcodes[1] = 641;
        s.active_gcodes[2] = 10;
        s.active_gcodes[3] = 80;
        s.active_mcodes[1] = 5;
        s.active_mcodes[2] = 0;
        assert_eq!(s.modal_string(), "G1 G64.1 M5");
    }

    #[test]
    fn work_position_subtracts_offsets() {
        let mut s = Status::default();
        s.actual[Axis::X] = 100.0;
        s.g5x_offset[0] = 30.0;
        s.g92_offset[0] = 5.0;
        s.tool_offset[0] = 1.0;
        assert_eq!(s.work_position()[Axis::X], 64.0);
    }

    #[test]
    fn finished_waits_for_echo_and_exec() {
        let mut s = Status { echo_serial: Serial(4), command_status: RcsStatus::Exec, ..Status::default() };
        assert!(!s.finished(Serial(4)));
        s.command_status = RcsStatus::Done;
        assert!(s.finished(Serial(4)));
        assert!(!s.finished(Serial(5)));
    }
}
