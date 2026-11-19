//! ferrum's machine layer: the [`Machine`] trait over LinuxCNC (through the
//! `lcnc` crate) or the in-process simulator, and the worker thread that owns
//! the connection and applies ferrum's mode rules.

mod sim;
mod worker;

pub use lcnc::*;
/// Older names used throughout ferrum.
pub use lcnc::{InterpState as Interp, TaskMode as Mode};
pub use sim::Sim;
pub use worker::{keep_modes, set_axis_joints, spawn, Event, Handle, KEEP_MODES};
#[cfg(target_os = "wasi")]
pub use worker::pump;

pub const AXIS_LETTERS: [char; AXES] = Axis::LETTERS;

/// What the screens get from the worker: LinuxCNC's status plus what only
/// the simulator reports. Derefs to [`lcnc::Status`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub lcnc: lcnc::Status,
    /// Simulator only: the tool an M6 is waiting for. On LinuxCNC the request
    /// arrives on HAL (iocontrol.0.tool-change) and the GUI answers there.
    pub sim_tool_change: Option<i32>,
}

impl std::ops::Deref for Status {
    type Target = lcnc::Status;
    fn deref(&self) -> &lcnc::Status {
        &self.lcnc
    }
}

impl std::ops::DerefMut for Status {
    fn deref_mut(&mut self) -> &mut lcnc::Status {
        &mut self.lcnc
    }
}

impl From<lcnc::Status> for Status {
    fn from(lcnc: lcnc::Status) -> Self {
        Self { lcnc, sim_tool_change: None }
    }
}

/// A connection to LinuxCNC (or a stand-in). Used from one thread.
pub trait Machine: Send {
    /// The newest status. Borrowed: the worker copies it only when it changed.
    fn poll(&mut self) -> Result<&lcnc::Status>;
    fn send(&mut self, cmd: &Command) -> Result<Serial>;
    fn next_message(&mut self) -> Option<Message>;
    /// Simulator only: the tool an M6 is waiting for.
    fn sim_tool_change(&self) -> Option<i32> {
        None
    }
    /// Simulator only: finish the pending M6 (LinuxCNC gets this over HAL).
    fn sim_tool_changed(&mut self) {}
}

impl Machine for Connection {
    fn poll(&mut self) -> Result<&lcnc::Status> {
        Connection::poll(self)
    }
    fn send(&mut self, cmd: &Command) -> Result<Serial> {
        Connection::send(self, cmd)
    }
    fn next_message(&mut self) -> Option<Message> {
        Connection::next_message(self)
    }
}

/// Connect to the running LinuxCNC over NML.
pub fn connect(nml_file: Option<&str>) -> Result<Box<dyn Machine>, String> {
    Connection::open(nml_file.map(std::path::Path::new)).map(|c| Box::new(c) as Box<dyn Machine>).map_err(|e| e.to_string())
}
