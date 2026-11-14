//! The JOB page and the conversational generators for millgui, without
//! Python.
//!
//! [`Helper`] answers the JSON requests millgui used to send to
//! `job_helper.py`, with the same replies. The modules below it are ports of
//! the Python it imported:
//!
//! * [`model`]: `mymill_job/model.py` (the job file)
//! * [`compiler`]: `mymill_job/compiler.py` and `conversational.py`
//! * [`gcode`]: `mymill_job/gcode.py`
//! * [`session`]: `mymill_vcp/job_session.py` (stamp, skip pins, stop after)
//! * [`mill_ops`]: probe_basic's `mill_ops.py` and `thread_presets.py`
//! * [`py`]: the Python formatting, JSON and error text they depend on

pub mod compiler;
pub mod fusion;
pub mod gcode;
pub mod helper;
pub mod mill_ops;
pub mod model;
pub mod py;
pub mod session;

pub use helper::{tidy, Helper};
pub use py::Error;
pub use session::Hooks;
