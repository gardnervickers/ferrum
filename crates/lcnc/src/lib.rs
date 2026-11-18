//! Typed Rust bindings for [LinuxCNC](https://linuxcnc.org) 2.9.
//!
//! - [`Connection`]: the NML status, command and error channels, what the
//!   Python `linuxcnc` module offers as `stat`, `command` and
//!   `error_channel`. No Python is involved.
//! - [`Status`]: one typed snapshot of `EMC_STAT`, with enums for states and
//!   modes, [`Pose`] for positions and [`ActiveGCodes`] for modal codes.
//! - [`Command`]: every command as an enum variant, so a jog target, a
//!   joint selection or a spindle direction can't be passed as a bare
//!   number.
//! - [`hal`]: userspace HAL components with pins typed by value and
//!   direction (`In<bool>` has no `set`).
//!
//! ```no_run
//! use lcnc::{Command, Connection, TaskMode, TaskState};
//! use std::time::Duration;
//!
//! let mut lc = Connection::open(None)?;
//! let s = lc.poll()?;
//! println!("{:?} in {:?}, X {:.3}", s.task_state, s.mode, s.work_position()[lcnc::Axis::X]);
//!
//! if s.power_on() && !s.busy() {
//!     let serial = lc.send(&Command::SetMode(TaskMode::Mdi))?;
//!     lc.wait_finished(serial, Duration::from_secs(1))?;
//!     let serial = lc.send(&Command::Mdi("G0 X10".into()))?;
//!     lc.wait_finished(serial, Duration::from_secs(30))?;
//! }
//! while let Some(m) = lc.next_message() {
//!     eprintln!("{:?}: {}", m.kind, m.text);
//! }
//! # Ok::<(), lcnc::Error>(())
//! ```
//!
//! # Building
//!
//! The NML side is a small C++ shim compiled against LinuxCNC's headers
//! (`linuxcnc-uspace-dev`), linked to `libnml` and `liblinuxcncini`; see
//! `build.rs` for where headers are looked for. Without them (or on a
//! non-Linux host) the crate still builds: every type is there, and
//! [`Connection::open`] returns [`Error::NoBackend`]. That keeps code that
//! uses the types buildable and testable on a laptop.
//!
//! The HAL side needs nothing at build time; `liblinuxcnchal.so.0` is
//! loaded when the first component is made.
//!
//! # Threads
//!
//! [`Connection`] is `Send` but not `Sync`. LinuxCNC's GUIs poll status
//! every 50 to 100 ms from one thread and send commands from the same
//! thread; do the same, and hand snapshots to other threads by value.

mod command;
mod error;
mod status;
mod types;

#[cfg(feature = "hal")]
pub mod hal;

#[cfg(lcnc_nml)]
mod nml;

pub use command::{Command, JogTarget, JointSel, Rotation};
pub use error::{Error, Result};
pub use status::Status;
pub use types::*;

#[cfg(lcnc_nml)]
pub use nml::Connection;

/// Built without the NML backend: [`open`](Connection::open) always fails
/// with [`Error::NoBackend`], so code using it still compiles.
#[cfg(not(lcnc_nml))]
pub struct Connection {
    status: Status,
}

#[cfg(not(lcnc_nml))]
impl Connection {
    pub fn open(_nml_file: Option<&std::path::Path>) -> Result<Self> {
        Err(Error::NoBackend)
    }
    pub fn poll(&mut self) -> Result<&Status> {
        Err(Error::NoBackend)
    }
    pub fn status(&self) -> &Status {
        &self.status
    }
    pub fn send(&mut self, _cmd: &Command) -> Result<Serial> {
        Err(Error::NoBackend)
    }
    pub fn wait_finished(&mut self, _serial: Serial, _timeout: std::time::Duration) -> Result<()> {
        Err(Error::NoBackend)
    }
    pub fn next_message(&mut self) -> Option<Message> {
        None
    }
}

/// True when this build can talk to LinuxCNC (the NML shim was compiled).
pub const HAS_NML: bool = cfg!(lcnc_nml);
