//! LinuxCNC machine interface for millgui: a status snapshot, commands and
//! operator messages behind the `Machine` trait, with an NML backend on the Pi
//! and a simulated machine everywhere.

mod sim;
mod status;
mod worker;

#[cfg(has_nml)]
mod nml;

pub use sim::Sim;
pub use status::*;
pub use worker::{spawn, Event, Handle};

#[cfg(has_nml)]
pub use nml::Nml;

/// Connect to the running LinuxCNC over NML.
pub fn connect(nml_file: Option<&str>) -> Result<Box<dyn Machine>, String> {
    #[cfg(has_nml)]
    {
        Nml::open(nml_file).map(|m| Box::new(m) as Box<dyn Machine>)
    }
    #[cfg(not(has_nml))]
    {
        let _ = nml_file;
        Err("built without the NML backend (not a Pi build)".into())
    }
}
