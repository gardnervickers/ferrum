//! Small typed values used by [`Status`](crate::Status) and
//! [`Command`](crate::Command): states, modes, axes, poses and G/M codes.

use std::fmt;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::slice::SliceIndex;

/// Declares an enum over one of LinuxCNC's integer codes. Values this crate
/// does not know decode to `Unknown(raw)` instead of being guessed, so a
/// newer LinuxCNC can't be misread silently.
macro_rules! raw_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident default $default:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $raw:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this crate doesn't know (from a newer LinuxCNC, or
            /// a status that was never filled in).
            Unknown(i32),
        }

        impl $name {
            /// Decodes LinuxCNC's integer code.
            pub const fn from_raw(raw: i32) -> Self {
                match raw {
                    $( $raw => Self::$variant, )+
                    other => Self::Unknown(other),
                }
            }

            /// LinuxCNC's integer code.
            pub const fn raw(self) -> i32 {
                match self {
                    $( Self::$variant => $raw, )+
                    Self::Unknown(raw) => raw,
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$default
            }
        }

        impl From<i32> for $name {
            fn from(raw: i32) -> Self {
                Self::from_raw(raw)
            }
        }
    };
}

raw_enum! {
    /// `EMC_TASK_STATE`: E-stop and machine power.
    pub enum TaskState default Estop {
        Estop = 1,
        EstopReset = 2,
        Off = 3,
        On = 4,
    }
}

raw_enum! {
    /// `EMC_TASK_MODE`: which kind of command task accepts.
    pub enum TaskMode default Manual {
        Manual = 1,
        Auto = 2,
        Mdi = 3,
    }
}

raw_enum! {
    /// `EMC_TASK_INTERP`: what the interpreter is doing.
    pub enum InterpState default Idle {
        Idle = 1,
        Reading = 2,
        Paused = 3,
        Waiting = 4,
    }
}

raw_enum! {
    /// `EMC_TASK_EXEC`: what task is waiting for.
    pub enum ExecState default Done {
        Error = 1,
        Done = 2,
        WaitingForMotion = 3,
        WaitingForMotionQueue = 4,
        WaitingForIo = 5,
        WaitingForMotionAndIo = 7,
        WaitingForDelay = 8,
        WaitingForSystemCmd = 9,
        WaitingForSpindleOriented = 10,
    }
}

raw_enum! {
    /// `EMC_TRAJ_MODE`: free mode jogs joints, teleop jogs axes, coordinated
    /// runs programs and MDI.
    pub enum TrajMode default Free {
        Free = 1,
        Coord = 2,
        Teleop = 3,
    }
}

raw_enum! {
    /// `RCS_STATUS` of the last command task received.
    pub enum RcsStatus default Done {
        Uninitialized = -1,
        Done = 1,
        Exec = 2,
        Error = 3,
    }
}

raw_enum! {
    /// `EMC_MOTION_TYPE` of the move in progress.
    pub enum MotionType default None {
        None = 0,
        Traverse = 1,
        Feed = 2,
        Arc = 3,
        ToolChange = 4,
        Probing = 5,
        IndexRotary = 6,
    }
}

raw_enum! {
    /// Program units (G20/G21; centimetres are LinuxCNC-internal).
    pub enum ProgramUnits default Mm {
        Inch = 1,
        Mm = 2,
        Cm = 3,
    }
}

raw_enum! {
    /// Spindle 0's direction.
    pub enum SpindleDirection default Stopped {
        Reverse = -1,
        Stopped = 0,
        Forward = 1,
    }
}

impl SpindleDirection {
    /// -1, 0 or 1, for multiplying a speed.
    pub fn sign(self) -> f64 {
        match self {
            Self::Forward => 1.0,
            Self::Reverse => -1.0,
            _ => 0.0,
        }
    }
}

raw_enum! {
    /// The active work coordinate system (`g5x_index`).
    pub enum Wcs default G54 {
        G54 = 1,
        G55 = 2,
        G56 = 3,
        G57 = 4,
        G58 = 5,
        G59 = 6,
        G59_1 = 7,
        G59_2 = 8,
        G59_3 = 9,
    }
}

impl Wcs {
    /// All nine, in order.
    pub const ALL: [Wcs; 9] = [Wcs::G54, Wcs::G55, Wcs::G56, Wcs::G57, Wcs::G58, Wcs::G59, Wcs::G59_1, Wcs::G59_2, Wcs::G59_3];

    /// 0 for G54 up to 8 for G59.3 (the parameter block index, `P` - 1).
    pub fn index(self) -> Option<usize> {
        match self.raw() {
            r @ 1..=9 => Some(r as usize - 1),
            _ => None,
        }
    }

    /// The system at `index` (0 = G54), see [`Wcs::index`].
    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// "G54" ... "G59.3".
    pub fn name(self) -> &'static str {
        match self {
            Self::G54 => "G54",
            Self::G55 => "G55",
            Self::G56 => "G56",
            Self::G57 => "G57",
            Self::G58 => "G58",
            Self::G59 => "G59",
            Self::G59_1 => "G59.1",
            Self::G59_2 => "G59.2",
            Self::G59_3 => "G59.3",
            Self::Unknown(_) => "G5x?",
        }
    }
}

impl fmt::Display for Wcs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The kind of an operator message from the error channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// `(abort, ...)`, failed commands, NML errors.
    Error,
    /// `(msg, ...)` and operator text.
    Text,
    /// `(debug, ...)` and operator display messages.
    Display,
}

/// One operator message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub kind: MessageKind,
    pub text: String,
}

/// A command serial number. Task echoes the serial of the last command it
/// received in [`Status::echo_serial`](crate::Status::echo_serial).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Serial(pub i32);

impl fmt::Display for Serial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The nine axes LinuxCNC knows, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Axis {
    X = 0,
    Y = 1,
    Z = 2,
    A = 3,
    B = 4,
    C = 5,
    U = 6,
    V = 7,
    W = 8,
}

/// Number of axes in a [`Pose`].
pub const AXES: usize = 9;
/// Maximum joints reported in [`Status`](crate::Status) (`EMCMOT_MAX_JOINTS`).
pub const JOINTS: usize = 16;

impl Axis {
    pub const ALL: [Axis; AXES] = [Axis::X, Axis::Y, Axis::Z, Axis::A, Axis::B, Axis::C, Axis::U, Axis::V, Axis::W];
    pub const LETTERS: [char; AXES] = ['X', 'Y', 'Z', 'A', 'B', 'C', 'U', 'V', 'W'];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }

    pub fn letter(self) -> char {
        Self::LETTERS[self as usize]
    }

    /// Case-insensitive.
    pub fn from_letter(c: char) -> Option<Self> {
        let up = c.to_ascii_uppercase();
        Self::LETTERS.iter().position(|l| *l == up).and_then(Self::from_index)
    }

    /// A, B and C are angles; the rest are lengths.
    pub fn is_rotary(self) -> bool {
        matches!(self, Axis::A | Axis::B | Axis::C)
    }
}

impl fmt::Display for Axis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.letter())
    }
}

/// The axes a machine has (`[TRAJ]COORDINATES`), as LinuxCNC's axis mask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AxisMask(pub u32);

impl AxisMask {
    pub fn has(self, axis: Axis) -> bool {
        self.0 & (1 << axis as u32) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = Axis> {
        Axis::ALL.into_iter().filter(move |a| self.has(*a))
    }
}

/// A position in all nine axes (`EmcPose`), lengths in machine units and
/// angles in degrees. Index it by [`Axis`] or by number; it also derefs to
/// `[f64; 9]`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose(pub [f64; AXES]);

impl Pose {
    pub fn get(&self, axis: Axis) -> f64 {
        self.0[axis as usize]
    }
}

impl Deref for Pose {
    type Target = [f64; AXES];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Pose {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Index<Axis> for Pose {
    type Output = f64;
    fn index(&self, axis: Axis) -> &f64 {
        &self.0[axis as usize]
    }
}

impl IndexMut<Axis> for Pose {
    fn index_mut(&mut self, axis: Axis) -> &mut f64 {
        &mut self.0[axis as usize]
    }
}

impl<I: SliceIndex<[f64]>> Index<I> for Pose {
    type Output = I::Output;
    fn index(&self, i: I) -> &I::Output {
        &self.0[i]
    }
}

impl<I: SliceIndex<[f64]>> IndexMut<I> for Pose {
    fn index_mut(&mut self, i: I) -> &mut I::Output {
        &mut self.0[i]
    }
}

impl From<[f64; AXES]> for Pose {
    fn from(a: [f64; AXES]) -> Self {
        Self(a)
    }
}

impl PartialEq<[f64; AXES]> for Pose {
    fn eq(&self, other: &[f64; AXES]) -> bool {
        self.0 == *other
    }
}

/// A G code as LinuxCNC stores it: the number times ten, so G64.1 is
/// `GCode(641)` and G1 is `GCode(10)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GCode(pub i32);

impl GCode {
    /// From the number as written: `GCode::new(64.1)`.
    pub fn new(number: f64) -> Self {
        Self((number * 10.0).round() as i32)
    }
}

impl fmt::Display for GCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 % 10 == 0 {
            write!(f, "G{}", self.0 / 10)
        } else {
            write!(f, "G{}.{}", self.0 / 10, self.0 % 10)
        }
    }
}

/// An M code number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MCode(pub i32);

impl fmt::Display for MCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "M{}", self.0)
    }
}

/// Number of slots in [`ActiveGCodes`] (`ACTIVE_G_CODES`).
pub const GCODE_SLOTS: usize = 17;
/// Number of slots in [`ActiveMCodes`] (`ACTIVE_M_CODES`).
pub const MCODE_SLOTS: usize = 10;

/// The interpreter's active G codes, one slot per modal group. Slot 0 is the
/// block sequence number and unused slots hold -1. Derefs to the raw array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveGCodes(pub [i32; GCODE_SLOTS]);

impl Default for ActiveGCodes {
    fn default() -> Self {
        Self([-1; GCODE_SLOTS])
    }
}

impl ActiveGCodes {
    /// The active codes, in slot order.
    pub fn iter(&self) -> impl Iterator<Item = GCode> + '_ {
        self.0[1..].iter().filter(|g| **g >= 0).map(|g| GCode(*g))
    }

    pub fn has(&self, code: GCode) -> bool {
        self.0[1..].contains(&code.0)
    }

    /// The first active code of `set` (for example the distance mode out of
    /// `[G90, G91]`), whatever slot it is in.
    pub fn find(&self, set: &[GCode]) -> Option<GCode> {
        self.iter().find(|g| set.contains(g))
    }
}

impl Deref for ActiveGCodes {
    type Target = [i32; GCODE_SLOTS];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ActiveGCodes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The interpreter's active M codes, as [`ActiveGCodes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveMCodes(pub [i32; MCODE_SLOTS]);

impl Default for ActiveMCodes {
    fn default() -> Self {
        Self([-1; MCODE_SLOTS])
    }
}

impl ActiveMCodes {
    pub fn iter(&self) -> impl Iterator<Item = MCode> + '_ {
        self.0[1..].iter().filter(|m| **m >= 0).map(|m| MCode(*m))
    }

    pub fn has(&self, code: MCode) -> bool {
        self.0[1..].contains(&code.0)
    }
}

impl Deref for ActiveMCodes {
    type Target = [i32; MCODE_SLOTS];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ActiveMCodes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The interpreter's active settings: sequence number, feed rate, spindle
/// speed, G64 P tolerance and G64 Q naive-CAM tolerance.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ActiveSettings(pub [f64; 5]);

impl ActiveSettings {
    /// Programmed F, in program units per minute.
    pub fn feed(&self) -> f64 {
        self.0[1]
    }

    /// Programmed S.
    pub fn speed(&self) -> f64 {
        self.0[2]
    }
}

impl Deref for ActiveSettings {
    type Target = [f64; 5];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_codes_are_kept_not_guessed() {
        assert_eq!(TaskMode::from_raw(3), TaskMode::Mdi);
        assert_eq!(TaskMode::from_raw(7), TaskMode::Unknown(7));
        assert_eq!(TaskMode::Unknown(7).raw(), 7);
        assert_eq!(RcsStatus::from_raw(-1), RcsStatus::Uninitialized);
    }

    #[test]
    fn gcodes_print_like_written() {
        assert_eq!(GCode(641).to_string(), "G64.1");
        assert_eq!(GCode(10).to_string(), "G1");
        assert_eq!(GCode::new(59.3), GCode(593));
    }

    #[test]
    fn wcs_index_and_name() {
        assert_eq!(Wcs::G59_3.index(), Some(8));
        assert_eq!(Wcs::from_index(1), Some(Wcs::G55));
        assert_eq!(Wcs::Unknown(0).index(), None);
        assert_eq!(Wcs::G59_1.to_string(), "G59.1");
    }

    #[test]
    fn axes_and_masks() {
        assert_eq!(Axis::from_letter('a'), Some(Axis::A));
        let m = AxisMask(0b1111);
        assert_eq!(m.iter().collect::<Vec<_>>(), [Axis::X, Axis::Y, Axis::Z, Axis::A]);
        let mut p = Pose::default();
        p[Axis::Z] = -2.0;
        assert_eq!(p[2], -2.0);
    }
}
