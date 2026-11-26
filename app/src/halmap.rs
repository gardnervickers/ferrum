//! Connecting ferrum's HAL pins to the machine's, without a hand-written
//! HAL file. A module names each pin it can use with a logical name
//! (`probe_tripped`); `[hal]` in ferrum.toml maps the name to a signal or
//! pin of the machine, and some names have an obvious default
//! (`motion.probe-input`). A module with its own `ferrum/hal/<id>.hal`
//! beside the INI (the mymill profile) uses that file instead.

use std::process::Command;

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
    pin_full(pin).map(|(_, d, s)| (d, s))
}

/// (type, direction, signal) of a pin.
pub(crate) fn pin_full(pin: &str) -> Option<(String, String, String)> {
    let out = halcmd(&["-s", "show", "pin", pin]).ok()?;
    out.lines().find_map(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        let at = w.iter().position(|x| *x == pin)?;
        let typ = w.get(1)?.to_string();
        let dir = w.get(2)?.to_string();
        let sig = w.get(at + 2).filter(|_| w.get(at + 1).is_some_and(|a| a.contains("==") || *a == "<=>")).map_or(String::new(), |s| s.to_string());
        Some((typ, dir, sig))
    })
}

/// (type, has a writer) of a signal.
pub(crate) fn sig_info(name: &str) -> Option<(String, bool)> {
    let out = halcmd(&["-s", "show", "sig", name]).ok()?;
    let mut found = None;
    let mut written = false;
    for l in out.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.get(2) == Some(&name) {
            found = Some(w[0].to_string());
            written |= w.iter().any(|x| *x == "<==" || *x == "<=>");
        } else if found.is_some() && (w.first() == Some(&"<==") || w.first() == Some(&"<=>")) {
            // halcmd may list the pins on the lines after the signal.
            written = true;
        }
    }
    found.map(|t| (t, written))
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

/// What a target is, as HAL has it now.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    /// A signal: its type and whether something writes it.
    Signal { typ: String, written: bool },
    /// A pin: type, direction, and the signal it is on ("" for none).
    Pin { typ: String, dir: String, sig: String },
    Missing,
}

fn find(theirs: &str) -> Found {
    if let Some((typ, written)) = sig_info(theirs) {
        return Found::Signal { typ, written };
    }
    match pin_full(theirs) {
        Some((typ, dir, sig)) => Found::Pin { typ, dir, sig },
        None => Found::Missing,
    }
}

/// The outcome of connecting one pin.
#[derive(Clone, Debug, PartialEq)]
pub enum Linked {
    /// Connected; the text says how, for the log.
    Yes(String),
    /// Nothing there to read: a default target that nothing drives. The
    /// part counts as not fitted.
    Nothing(String),
}

/// Check that `ours` (type `ty`, ferrum reads it: `ours_in`) can connect to
/// what `theirs` is. `named`: the user named it in `[hal]` (a default that
/// is missing or undriven is quietly skipped instead).
pub fn check(ty: &str, ours_in: bool, theirs: &str, found: &Found, named: bool) -> Result<Option<String>, String> {
    match found {
        Found::Missing if named => Err(format!("HAL has no pin or signal named {theirs}")),
        Found::Missing => Ok(Some(format!("{theirs} is not in this machine's HAL"))),
        Found::Signal { typ, .. } | Found::Pin { typ, .. } if typ != ty => Err(format!("{theirs} is {typ}, the ferrum pin is {ty}")),
        Found::Signal { written, .. } if ours_in && !written && !named => Ok(Some(format!("nothing drives signal {theirs}"))),
        Found::Signal { written: true, .. } if !ours_in => Err(format!("signal {theirs} already has a writer; ferrum would be a second one")),
        Found::Pin { dir, sig, .. } if ours_in && dir == "IN" && sig.is_empty() && !named => Ok(Some(format!("{theirs} is not connected"))),
        Found::Pin { dir, sig, .. } if ours_in && dir == "IN" && !named => match sig_info(sig) {
            Some((_, false)) => Ok(Some(format!("nothing drives {theirs} (signal {sig})"))),
            _ => Ok(None),
        },
        Found::Pin { dir, .. } if !ours_in && dir == "OUT" => Err(format!("{theirs} is an output pin; ferrum's output cannot drive it")),
        Found::Pin { sig, .. } if !ours_in && !sig.is_empty() => match sig_info(sig) {
            Some((_, true)) => Err(format!("{theirs} is on signal {sig}, which already has a writer")),
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// Connect `ours` (a full pin name) to `theirs` (a signal or a pin).
fn connect(ours: &str, ours_in: bool, theirs: &str, found: &Found) -> Result<String, String> {
    let their = match found {
        Found::Signal { .. } => None,
        Found::Pin { dir, sig, .. } => Some((dir.clone(), sig.clone())),
        Found::Missing => return Err(format!("HAL has no pin or signal named {theirs}")),
    };
    match plan(ours, ours_in, their, theirs) {
        Plan::Join(sig) => halcmd(&["net", &sig, ours]).map(|_| format!("{ours} on signal {sig}")),
        Plan::New(sig) => halcmd(&["net", &sig, theirs, ours]).map(|_| format!("{ours} to {theirs} (signal {sig})")),
        Plan::Skip => Ok(format!("{ours}: {theirs} is not connected")),
    }
    .map_err(|e| format!("{ours} to {theirs}: {e}"))
}

struct Dyn<'a> {
    pin: &'a str,
    input: bool,
}

/// Load one realtime component instance on the servo thread.
fn load_instance(comp: &str, inst: &str) -> Result<(), String> {
    halcmd(&["loadrt", comp, &format!("names={inst}")]).map_err(|e| format!("loadrt {comp}: {e}"))?;
    halcmd(&["addf", inst, "servo-thread"]).map_err(|e| format!("addf {inst}: {e}"))?;
    Ok(())
}

/// Connect `ferrum.<pin>` per a `[hal]` entry or a default target, with
/// the entry's invert or scale done by a `not` or `scale` instance ferrum
/// loads itself (`ferrum-not-<pin>`, `ferrum-scale-<pin>`).
pub fn link(def: &crate::halpins::Def, t: &crate::machine_cfg::HalTarget, named: bool) -> Result<Linked, String> {
    link_pin(def.pin, def.ty, def.input, t, named)
}

/// As `link`, for a pin made at run time (an alarm's).
pub fn link_pin(pin: &str, ty: crate::halpins::Ty, input: bool, t: &crate::machine_cfg::HalTarget, named: bool) -> Result<Linked, String> {
    let def = Dyn { pin, input };
    let ours = format!("ferrum.{}", def.pin);
    let Some(theirs) = t.target.as_deref() else { return Ok(Linked::Nothing(format!("{ours}: none on this machine"))) };
    let ty = ty.as_str();
    if t.invert && ty != "bit" {
        return Err(format!("{theirs}: only a bit can be inverted (\"!\")"));
    }
    if t.scaled() && ty != "float" {
        return Err(format!("{theirs}: only a float can be scaled"));
    }
    let found = find(theirs);
    if let Some(why) = check(ty, def.input, theirs, &found, named)? {
        return Ok(Linked::Nothing(format!("{ours}: {why}")));
    }
    let tag = def.pin.replace('.', "-");
    let (via, comp, a, b) = if t.invert {
        (format!("ferrum-not-{tag}"), "not", "in", "out")
    } else if t.scaled() {
        (format!("ferrum-scale-{tag}"), "scale", "in", "out")
    } else {
        return connect(&ours, def.input, theirs, &found).map(Linked::Yes);
    };
    load_instance(comp, &via)?;
    if comp == "scale" {
        halcmd(&["setp", &format!("{via}.gain"), &t.scale.to_string()])?;
        halcmd(&["setp", &format!("{via}.offset"), &t.offset.to_string()])?;
    }
    let inner = format!("ferrum-{tag}");
    if def.input {
        // theirs -> via.in, via.out -> ours
        connect(&format!("{via}.{a}"), true, theirs, &found)?;
        halcmd(&["net", &inner, &format!("{via}.{b}"), &ours]).map_err(|e| format!("{ours}: {e}"))?;
    } else {
        // ours -> via.in, via.out -> theirs
        halcmd(&["net", &inner, &ours, &format!("{via}.{a}")]).map_err(|e| format!("{ours}: {e}"))?;
        connect(&format!("{via}.{b}"), false, theirs, &found)?;
    }
    Ok(Linked::Yes(format!("{ours} to {theirs} through {via}")))
}

/// One pin or signal, as `halcmd -s show pin` / `show sig` list them.
#[derive(Clone, Debug, PartialEq)]
pub struct HalItem {
    pub name: String,
    /// "bit", "float", "s32", "u32".
    pub typ: String,
    /// "IN", "OUT", "IO" for a pin; "" for a signal.
    pub dir: String,
    pub value: String,
    /// The pin's signal, "" when unconnected (pins only).
    pub sig: String,
    /// A signal has a writer (signals only).
    pub written: bool,
}

impl HalItem {
    pub fn is_signal(&self) -> bool {
        self.dir.is_empty()
    }
}

/// Every pin and signal, from `halcmd -s show pin` and `show sig` output.
pub fn parse_all(pins: &str, sigs: &str) -> Vec<HalItem> {
    let mut v = Vec::new();
    for l in pins.lines() {
        // owner type dir value name [arrow sig]
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() < 5 || !matches!(w[2], "IN" | "OUT" | "IO") {
            continue;
        }
        let sig = if w.len() >= 7 && w[5].contains("==") || w.len() >= 7 && w[5] == "<=>" { w[6].to_string() } else { String::new() };
        v.push(HalItem { name: w[4].into(), typ: w[1].into(), dir: w[2].into(), value: w[3].into(), sig, written: false });
    }
    for l in sigs.lines() {
        // type value name [==> pin | <== pin | <=> pin ...]
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() < 3 || !matches!(w[0], "bit" | "float" | "s32" | "u32" | "s64" | "u64" | "port") {
            continue;
        }
        let written = w.iter().any(|x| *x == "<==" || *x == "<=>");
        v.push(HalItem { name: w[2].into(), typ: w[0].into(), dir: String::new(), value: w[1].into(), sig: String::new(), written });
    }
    v
}

/// The whole HAL, or None when LinuxCNC is not running.
pub fn snapshot() -> Option<Vec<HalItem>> {
    let pins = halcmd(&["-s", "show", "pin"]).ok()?;
    let sigs = halcmd(&["-s", "show", "sig"]).ok()?;
    let all = parse_all(&pins, &sigs);
    (!all.is_empty()).then_some(all)
}

/// What a logical name may connect to: for a pin ferrum reads, signals of
/// its type and pins that write; for one ferrum drives, signals with no
/// writer and unconnected input pins. Signals first, then pins, by name.
pub fn candidates<'a>(all: &'a [HalItem], typ: &str, ferrum_reads: bool) -> Vec<&'a HalItem> {
    let mut v: Vec<&HalItem> = all
        .iter()
        .filter(|i| i.typ == typ && !i.name.starts_with("ferrum."))
        .filter(|i| {
            if i.is_signal() {
                ferrum_reads || !i.written
            } else if ferrum_reads {
                i.dir == "OUT" || i.dir == "IO" || !i.sig.is_empty()
            } else {
                i.dir == "IN" && i.sig.is_empty()
            }
        })
        .collect();
    v.sort_by(|a, b| (!a.is_signal(), &a.name).cmp(&(!b.is_signal(), &b.name)));
    v
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

    #[test]
    fn checks_direction_and_type() {
        let sig = |t: &str, w: bool| Found::Signal { typ: t.into(), written: w };
        let pin = |t: &str, d: &str, s: &str| Found::Pin { typ: t.into(), dir: d.into(), sig: s.into() };
        // Reading a driven signal, or an output pin: connect.
        assert_eq!(check("bit", true, "probe-in", &sig("bit", true), true), Ok(None));
        assert_eq!(check("bit", true, "x.out", &pin("bit", "OUT", ""), true), Ok(None));
        // Nothing drives it: not fitted, quietly.
        assert!(check("bit", true, "spindle-at-speed", &sig("bit", false), false).unwrap().unwrap().contains("nothing drives"));
        assert!(check("bit", true, "spindle.0.at-speed", &pin("bit", "IN", ""), false).unwrap().is_some());
        // Wrong type, a second writer, a missing name the user typed.
        assert!(check("bit", true, "vfd-amps", &sig("float", true), true).unwrap_err().contains("is float"));
        assert!(check("bit", false, "estop", &sig("bit", true), true).unwrap_err().contains("already has a writer"));
        assert!(check("bit", false, "x.out", &pin("bit", "OUT", ""), true).unwrap_err().contains("output pin"));
        assert!(check("bit", true, "nope", &Found::Missing, true).is_err());
        assert!(check("bit", true, "whb.pendant.is-connected", &Found::Missing, false).unwrap().is_some());
        // The user named it: connect even when nothing drives it yet (sets, a later file).
        assert_eq!(check("bit", true, "sim-probe", &sig("bit", false), true), Ok(None));
        // Driving a free input pin or an unwritten signal: connect.
        assert_eq!(check("bit", false, "estop-latch.0.fault-in", &pin("bit", "IN", ""), true), Ok(None));
        assert_eq!(check("bit", false, "spare", &sig("bit", false), true), Ok(None));
    }

    #[test]
    fn reads_halcmds_lists() {
        let pins = "and2 bit   IN FALSE and2.0.in0 <== foo2\nand2 bit   IN FALSE and2.0.in1\nand2 bit   OUT TRUE and2.0.out ==> foo\nsiggen float IN 1 siggen.0.amplitude\nferrum bit IN FALSE ferrum.probe.tripped\n";
        let sigs = "bit    TRUE  foo <== and2.0.out\nbit    FALSE  spare\n\n";
        let all = parse_all(pins, sigs);
        assert_eq!(all.len(), 7);
        assert_eq!(all[0].sig, "foo2");
        assert_eq!(all[2].value, "TRUE");
        assert!(all[5].written && !all[6].written);
        let reads: Vec<&str> = candidates(&all, "bit", true).iter().map(|i| i.name.as_str()).collect();
        assert_eq!(reads, ["foo", "spare", "and2.0.in0", "and2.0.out"]);
        let drives: Vec<&str> = candidates(&all, "bit", false).iter().map(|i| i.name.as_str()).collect();
        assert_eq!(drives, ["spare", "and2.0.in1"]);
    }
}
