//! Print LinuxCNC's status once, then every operator message as it comes.
//!
//!     cargo run -p lcnc --example status [path/to/linuxcnc.nml]
use lcnc::{Axis, Connection};
use std::time::Duration;

fn main() -> Result<(), lcnc::Error> {
    let nml = std::env::args().nth(1);
    let mut lc = Connection::open(nml.as_deref().map(std::path::Path::new))?;
    let s = lc.poll()?;
    println!("state {:?}, mode {:?}, interp {:?}, {} homed: {}", s.task_state, s.mode, s.interp, s.wcs, s.all_homed());
    let w = s.work_position();
    let axes: Vec<String> = s.axis_mask.iter().map(|a: Axis| format!("{a} {:.3}", w[a])).collect();
    println!("{}", axes.join("  "));
    println!("T{}  {}  file {:?}", s.tool_in_spindle, s.modal_string(), s.file);
    loop {
        while let Some(m) = lc.next_message() {
            println!("{:?}: {}", m.kind, m.text);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
