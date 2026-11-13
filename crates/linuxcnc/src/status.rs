//! Machine-independent view of LinuxCNC status and commands.

pub const AXES: usize = 9;
pub const AXIS_LETTERS: [char; AXES] = ['X', 'Y', 'Z', 'A', 'B', 'C', 'U', 'V', 'W'];
pub const JOINTS: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskState {
    #[default]
    Estop = 1,
    EstopReset = 2,
    Off = 3,
    On = 4,
}

impl TaskState {
    pub fn from_raw(v: i32) -> Self {
        match v {
            2 => Self::EstopReset,
            3 => Self::Off,
            4 => Self::On,
            _ => Self::Estop,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Manual = 1,
    Auto = 2,
    Mdi = 3,
}

impl Mode {
    pub fn from_raw(v: i32) -> Self {
        match v {
            2 => Self::Auto,
            3 => Self::Mdi,
            _ => Self::Manual,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Interp {
    #[default]
    Idle = 1,
    Reading = 2,
    Paused = 3,
    Waiting = 4,
}

impl Interp {
    pub fn from_raw(v: i32) -> Self {
        match v {
            2 => Self::Reading,
            3 => Self::Paused,
            4 => Self::Waiting,
            _ => Self::Idle,
        }
    }
}

/// Snapshot of the status channel. Positions are machine coordinates in the
/// machine's linear units (mm here) and degrees.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub task_state: TaskState,
    pub mode: Mode,
    pub interp: Interp,
    pub exec_state: i32,
    pub enabled: bool,
    pub joints: usize,
    pub axis_mask: u32,
    pub homed: [bool; JOINTS],
    pub homing: [bool; JOINTS],
    pub actual: [f64; AXES],
    pub commanded: [f64; AXES],
    pub dtg: [f64; AXES],
    pub g5x_offset: [f64; AXES],
    pub g92_offset: [f64; AXES],
    pub tool_offset: [f64; AXES],
    pub rotation_xy: f64,
    pub g5x_index: i32,
    pub tool_in_spindle: i32,
    pub feed_scale: f64,
    pub rapid_scale: f64,
    pub spindle_scale: f64,
    /// Current tool tip speed in units per second.
    pub current_vel: f64,
    pub spindle_speed: f64,
    pub spindle_direction: i32,
    pub spindle_enabled: bool,
    pub active_gcodes: [i32; 17],
    pub active_mcodes: [i32; 10],
    pub active_settings: [f64; 5],
    pub file: String,
    pub current_line: i32,
    pub motion_line: i32,
    pub read_line: i32,
    pub optional_stop: bool,
    pub block_delete: bool,
    pub paused: bool,
    pub queued_mdi: i32,
    pub probe_tripped: bool,
    pub probing: bool,
    pub on_soft_limit: bool,
    pub echo_serial: i32,
    pub command_status: i32,
}

impl Status {
    pub fn estop(&self) -> bool {
        self.task_state == TaskState::Estop
    }

    pub fn power_on(&self) -> bool {
        self.task_state == TaskState::On
    }

    pub fn has_axis(&self, axis: usize) -> bool {
        self.axis_mask & (1 << axis) != 0
    }

    pub fn all_homed(&self) -> bool {
        self.joints > 0 && self.homed[..self.joints.min(JOINTS)].iter().all(|h| *h)
    }

    /// True while a program or MDI command is running or paused.
    pub fn busy(&self) -> bool {
        self.interp != Interp::Idle
    }

    /// Work (displayed) position, the way LinuxCNC's own GUIs compute it:
    /// subtract tool offset, then G5x, rotate by -rotation_xy, subtract G92.
    pub fn work_position(&self) -> [f64; AXES] {
        let mut p = [0.0; AXES];
        for i in 0..AXES {
            p[i] = self.actual[i] - self.tool_offset[i] - self.g5x_offset[i];
        }
        if self.rotation_xy != 0.0 {
            let t = (-self.rotation_xy).to_radians();
            let (x, y) = (p[0], p[1]);
            p[0] = x * t.cos() - y * t.sin();
            p[1] = x * t.sin() + y * t.cos();
        }
        for i in 0..AXES {
            p[i] -= self.g92_offset[i];
        }
        p
    }

    pub fn wcs_name(&self) -> &'static str {
        match self.g5x_index {
            1 => "G54",
            2 => "G55",
            3 => "G56",
            4 => "G57",
            5 => "G58",
            6 => "G59",
            7 => "G59.1",
            8 => "G59.2",
            9 => "G59.3",
            _ => "G5x?",
        }
    }

    /// Active modal codes as "G1 G17 ... M5 M9", sorted. LinuxCNC stores G
    /// codes as code * 10 (G64.1 = 641) and -1 for unused; index 0 is the
    /// block sequence. Lathe diameter/radius mode (G7/G8) and the stop slot
    /// (M0/M1/M2/M30/M60) are left out on a mill.
    pub fn modal_string(&self) -> String {
        let mut g: Vec<i32> = self.active_gcodes[1..].iter().copied().filter(|&g| g >= 0 && g != 70 && g != 80).collect();
        g.sort_unstable();
        g.dedup();
        let mut m: Vec<i32> =
            self.active_mcodes[1..].iter().copied().filter(|&m| m >= 0 && ![0, 1, 2, 30, 60].contains(&m)).collect();
        m.sort_unstable();
        m.dedup();
        g.iter()
            .map(|g| if g % 10 == 0 { format!("G{}", g / 10) } else { format!("G{}.{}", g / 10, g % 10) })
            .chain(m.iter().map(|m| format!("M{m}")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Tool tip feed in units per minute.
    pub fn feed_per_min(&self) -> f64 {
        self.current_vel * 60.0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetState(TaskState),
    SetMode(Mode),
    Abort,
    /// Runs in MDI mode; the worker switches mode first when idle.
    Mdi(String),
    ProgramOpen(String),
    /// Run from this line (0 = start). The worker switches to auto first.
    ProgramRun(i32),
    Pause,
    Resume,
    Step,
    OptionalStop(bool),
    BlockDelete(bool),
    /// Joint number, -1 for all.
    Home(i32),
    Unhome(i32),
    Teleop(bool),
    JogCont { joint_mode: bool, index: i32, vel: f64 },
    JogIncr { joint_mode: bool, index: i32, vel: f64, incr: f64 },
    JogStop { joint_mode: bool, index: i32 },
    FeedScale(f64),
    RapidScale(f64),
    SpindleScale(f64),
    /// Negative rpm runs counter-clockwise.
    SpindleOn(f64),
    SpindleOff,
    LoadToolTable,
    PlanSynch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageKind {
    Error,
    Text,
    Display,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub kind: MessageKind,
    pub text: String,
}

/// A connection to LinuxCNC (or a stand-in). Used from one thread.
pub trait Machine: Send {
    fn poll(&mut self) -> Result<Status, String>;
    /// Returns the command serial number.
    fn send(&mut self, cmd: &Command) -> Result<i32, String>;
    fn next_message(&mut self) -> Option<Message>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_string_formats_decimal_codes() {
        let mut s = Status::default();
        s.active_gcodes = [-1; 17];
        s.active_gcodes[1] = 641;
        s.active_gcodes[2] = 10;
        s.active_gcodes[3] = 80;
        s.active_mcodes = [-1; 10];
        s.active_mcodes[1] = 5;
        s.active_mcodes[2] = 0;
        assert_eq!(s.modal_string(), "G1 G64.1 M5");
    }

    #[test]
    fn work_position_subtracts_offsets() {
        let mut s = Status::default();
        s.actual[0] = 100.0;
        s.g5x_offset[0] = 30.0;
        s.g92_offset[0] = 5.0;
        s.tool_offset[0] = 1.0;
        assert_eq!(s.work_position()[0], 64.0);
    }
}
