//! Commands for LinuxCNC's command channel.

use crate::types::*;
use std::path::PathBuf;

/// Which joints a home or unhome applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JointSel {
    All,
    Joint(u8),
}

impl JointSel {
    /// LinuxCNC's encoding: -1 for all.
    pub fn raw(self) -> i32 {
        match self {
            Self::All => -1,
            Self::Joint(j) => j as i32,
        }
    }

    pub fn includes(self, joint: usize) -> bool {
        match self {
            Self::All => true,
            Self::Joint(j) => j as usize == joint,
        }
    }
}

/// What a jog moves. LinuxCNC 2.9 jogs joints in free mode (before homing)
/// and axes in teleop mode (after); task refuses the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JogTarget {
    Axis(Axis),
    Joint(u8),
}

/// Spindle rotation for [`Command::SpindleOn`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    /// M3, clockwise.
    #[default]
    Forward,
    /// M4, counter-clockwise.
    Reverse,
}

/// A command for task. Each is one NML message; none waits for the
/// command to finish (see [`Status::finished`](crate::Status::finished)).
///
/// Task checks every command against its mode and state and reports a
/// refusal on the error channel, not as a send failure: for example MDI
/// only runs in [`TaskMode::Mdi`], jogs and homing only in
/// [`TaskMode::Manual`].
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetState(TaskState),
    SetMode(TaskMode),
    /// Stop everything: motion, the interpreter, MDI.
    Abort,
    /// One line of G-code, in MDI mode.
    Mdi(String),
    /// Close any open program, then open this one.
    ProgramOpen(PathBuf),
    /// Run the open program from this line (0 = the start), in auto mode.
    ProgramRun(u32),
    Pause,
    Resume,
    /// Run one line (single block).
    Step,
    OptionalStop(bool),
    BlockDelete(bool),
    Home(JointSel),
    Unhome(JointSel),
    /// Teleop on (axis jogs) or off (free mode, joint jogs, homing).
    Teleop(bool),
    /// Jog until [`Command::JogStop`]; `vel` in units per second, signed.
    JogCont { target: JogTarget, vel: f64 },
    /// Jog `incr` units at `vel` units per second.
    JogIncr { target: JogTarget, vel: f64, incr: f64 },
    JogStop { target: JogTarget },
    /// Feed override, 1.0 = 100 %.
    FeedScale(f64),
    RapidScale(f64),
    /// Spindle 0 override.
    SpindleScale(f64),
    /// Start spindle 0 and wait for it to reach speed before feed moves.
    SpindleOn { rpm: f64, rotation: Rotation },
    SpindleOff,
    SpindleBrake(bool),
    /// Allowed in any mode, also while a program runs.
    Mist(bool),
    /// Allowed in any mode, also while a program runs.
    Flood(bool),
    /// Maximum tool tip velocity, units per second.
    MaxVelocity(f64),
    FeedOverrideEnable(bool),
    SpindleOverrideEnable(bool),
    FeedHoldEnable(bool),
    /// Reload the tool table file.
    LoadToolTable,
    /// Make the interpreter re-read its state from task (after an outside
    /// change such as a tool table reload).
    PlanSynch,
}

impl Command {
    /// A short name for messages ("SpindleOn").
    pub fn name(&self) -> &'static str {
        match self {
            Self::SetState(_) => "SetState",
            Self::SetMode(_) => "SetMode",
            Self::Abort => "Abort",
            Self::Mdi(_) => "Mdi",
            Self::ProgramOpen(_) => "ProgramOpen",
            Self::ProgramRun(_) => "ProgramRun",
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::Step => "Step",
            Self::OptionalStop(_) => "OptionalStop",
            Self::BlockDelete(_) => "BlockDelete",
            Self::Home(_) => "Home",
            Self::Unhome(_) => "Unhome",
            Self::Teleop(_) => "Teleop",
            Self::JogCont { .. } => "JogCont",
            Self::JogIncr { .. } => "JogIncr",
            Self::JogStop { .. } => "JogStop",
            Self::FeedScale(_) => "FeedScale",
            Self::RapidScale(_) => "RapidScale",
            Self::SpindleScale(_) => "SpindleScale",
            Self::SpindleOn { .. } => "SpindleOn",
            Self::SpindleOff => "SpindleOff",
            Self::SpindleBrake(_) => "SpindleBrake",
            Self::Mist(_) => "Mist",
            Self::Flood(_) => "Flood",
            Self::MaxVelocity(_) => "MaxVelocity",
            Self::FeedOverrideEnable(_) => "FeedOverrideEnable",
            Self::SpindleOverrideEnable(_) => "SpindleOverrideEnable",
            Self::FeedHoldEnable(_) => "FeedHoldEnable",
            Self::LoadToolTable => "LoadToolTable",
            Self::PlanSynch => "PlanSynch",
        }
    }
}
