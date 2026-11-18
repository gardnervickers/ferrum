//! A tiny HAL component: `out` follows `in` times `gain`.
//! Run inside a LinuxCNC session (halrun), then `halcmd show pin demo`.
use lcnc::hal::{Component, In, Out};
use std::time::Duration;

fn main() -> Result<(), lcnc::Error> {
    let comp = Component::new("demo")?;
    let input: In<f64> = comp.pin("in")?;
    let gain: In<f64> = comp.pin("gain")?;
    let output: Out<f64> = comp.pin("out")?;
    let alive: Out<bool> = comp.pin("alive")?;
    comp.ready()?;
    loop {
        output.set(input.get() * gain.get());
        alive.set(!alive.get());
        std::thread::sleep(Duration::from_millis(10));
    }
}
