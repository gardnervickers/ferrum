//! Connecting ferrum's HAL pins to the machine's, without a hand-written
//! HAL file. A module names each pin it can use with a logical name
//! (`probe_tripped`); `[hal]` in ferrum.toml maps the name to a signal or
//! pin of the machine, and some names have an obvious default
//! (`motion.probe-input`). A module with its own `ferrum/hal/<id>.hal`
//! beside the INI (the mymill profile) uses that file instead.

use std::process::Command;

/// A pin a module can connect: `ferrum.<pin>`, `[hal]<name>`, default target.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub name: &'static str,
    pub pin: &'static str,
    /// ferrum reads the pin (an IN pin).
    pub input: bool,
    pub auto: Option<&'static str>,
}

pub(crate) fn halcmd(args: &[&str]) -> Result<String, String> {
    let out = Command::new("halcmd").args(args).output().map_err(|e| format!("halcmd: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let e = String::from_utf8_lossy(&out.stderr);
        Err(e.lines().find(|l| !l.trim().is_empty()).unwrap_or("halcmd failed").trim().to_string())
    }
}

/// What `halcmd -s show pin` says about a pin: (direction, signal).
pub(crate) fn pin_info(pin: &str) -> Option<(String, String)> {
    let out = halcmd(&["-s", "show", "pin", pin]).ok()?;
    out.lines().find_map(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        let at = w.iter().position(|x| *x == pin)?;
        let dir = w.get(2)?.to_string();
        let sig = w.get(at + 2).filter(|_| w.get(at + 1).is_some_and(|a| a.contains("=="))).map_or(String::new(), |s| s.to_string());
        Some((dir, sig))
    })
}

pub(crate) fn is_signal(name: &str) -> bool {
    halcmd(&["-s", "show", "sig", name]).is_ok_and(|o| o.lines().any(|l| l.split_whitespace().nth(2) == Some(name)))
}

/// What a link should do, from what HAL has.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// `net <sig> <ours>`: join an existing signal.
    Join(String),
    /// `net <new> <theirs> <ours>`: a new signal between two pins.
    New(String),
    /// Nothing to read: the target is an unconnected input, like ours.
    Skip,
}

/// Decide how to connect `ours` (an IN or OUT pin of ferrum) to
/// `theirs`, which `their` describes: None for a signal, Some((dir, sig))
/// for a pin.
pub fn plan(ours: &str, ours_in: bool, their: Option<(String, String)>, theirs: &str) -> Plan {
    match their {
        None => Plan::Join(theirs.to_string()),
        Some((_, sig)) if !sig.is_empty() => Plan::Join(sig),
        Some((dir, _)) if ours_in && dir == "IN" => Plan::Skip,
        Some(_) => Plan::New(format!("ferrum-{}", ours.trim_start_matches("ferrum.").replace('.', "-"))),
    }
}

/// Connect `ferrum.<pin>` to `theirs` (a signal or a pin). Returns what
/// was done, for the log.
pub fn link(pin: &str, ours_in: bool, theirs: &str) -> Result<String, String> {
    let ours = format!("ferrum.{pin}");
    let their = if is_signal(theirs) {
        None
    } else {
        Some(pin_info(theirs).ok_or_else(|| format!("HAL has no pin or signal named {theirs}"))?)
    };
    match plan(&ours, ours_in, their, theirs) {
        Plan::Join(sig) => halcmd(&["net", &sig, &ours]).map(|_| format!("{ours} on signal {sig}")),
        Plan::New(sig) => halcmd(&["net", &sig, theirs, &ours]).map(|_| format!("{ours} to {theirs} (signal {sig})")),
        Plan::Skip => Ok(format!("{ours}: {theirs} is not connected")),
    }
    .map_err(|e| format!("{ours} to {theirs}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans() {
        assert_eq!(plan("ferrum.probe.tripped", true, None, "probe-in"), Plan::Join("probe-in".into()));
        let pin = |d: &str, s: &str| Some((d.to_string(), s.to_string()));
        assert_eq!(plan("ferrum.probe.tripped", true, pin("IN", "sim-probe"), "motion.probe-input"), Plan::Join("sim-probe".into()));
        assert_eq!(plan("ferrum.probe.tripped", true, pin("IN", ""), "motion.probe-input"), Plan::Skip);
        assert_eq!(
            plan("ferrum.toolchange.tool-changed", false, pin("IN", ""), "iocontrol.0.tool-changed"),
            Plan::New("ferrum-toolchange-tool-changed".into())
        );
        assert_eq!(plan("ferrum.toolchange.tool-change", true, pin("OUT", ""), "iocontrol.0.tool-change"), Plan::New("ferrum-toolchange-tool-change".into()));
    }
}
