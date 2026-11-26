//! Every HAL pin ferrum makes, in one table: the HAL contract.
//!
//! Each `ferrum.<pin>` exists on every start, whatever modules are on, so a
//! machine's own HAL file can always net it. A pin with a logical `name`
//! connects without a HAL file: `[hal]<name>` in ferrum.toml names a signal
//! or pin of the machine, else the pin's `auto` target, a standard LinuxCNC
//! pin whose signal ferrum joins (crate::halmap). Only the pins of modules
//! that run are connected.
//!
//! Three tiers (docs/hal.md):
//! - required: the screen watchdog (on unless `[machine]screen_watchdog = false`);
//! - standard: `auto` is a motion or iocontrol pin every config has;
//! - optional: no `auto`; off until `[hal]` names the machine's signal.

/// HAL types ferrum uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Bit,
    Float,
    S32,
}

impl Ty {
    pub fn as_str(self) -> &'static str {
        match self {
            Ty::Bit => "bit",
            Ty::Float => "float",
            Ty::S32 => "s32",
        }
    }
}

/// One ferrum pin.
#[derive(Clone, Copy, Debug)]
pub struct Def {
    /// The pin after `ferrum.`.
    pub pin: &'static str,
    pub ty: Ty,
    /// ferrum reads it (an IN pin).
    pub input: bool,
    /// The `[hal]` name; None for pins only a HAL file nets.
    pub name: Option<&'static str>,
    /// A LinuxCNC pin to join when `[hal]` does not name one.
    pub auto: Option<&'static str>,
    /// The modules that use it (connected when one of them runs).
    pub modules: &'static [&'static str],
    /// Only on machines with this axis.
    pub axis: Option<char>,
    /// What it is, for SETUP › HAL WIRING and `--hal-pins`.
    pub title: &'static str,
}

const fn d(pin: &'static str, ty: Ty, input: bool, name: &'static str, auto: Option<&'static str>, modules: &'static [&'static str], title: &'static str) -> Def {
    Def { pin, ty, input, name: Some(name), auto, modules, axis: None, title }
}

const fn ax(pin: &'static str, name: &'static str, axis: char, title: &'static str) -> Def {
    Def { pin, ty: Ty::Bit, input: true, name: Some(name), auto: None, modules: &["drives"], axis: Some(axis), title }
}

use Ty::*;

const TC: &[&str] = &["toolchange", "drawbar"];
const IN: bool = true;
const OUT: bool = false;

/// The whole contract, grouped by module in SETUP's order.
pub const ALL: &[Def] = &[
    // Machine signals (always on).
    d("spindle.speed-actual", Float, IN, "spindle_speed_actual", Some("spindle.0.speed-in"), &["machine"], "Spindle speed feedback, rev/s"),
    d("spindle.at-speed", Bit, IN, "spindle_at_speed", Some("spindle.0.at-speed"), &["machine"], "Spindle is at speed"),
    d("spindle.inhibited", Bit, IN, "spindle_inhibited", Some("spindle.0.inhibit"), &["machine"], "Spindle is inhibited"),
    d("spindle.fault", Bit, IN, "spindle_fault", None, &["machine"], "Spindle drive (VFD) fault"),
    d("machine.lube-ok", Bit, IN, "lube_ok", Some("iocontrol.0.lube_level"), &["machine"], "Lube level is OK"),
    // Probe and tool setter.
    d("probe.tripped", Bit, IN, "probe_tripped", Some("motion.probe-input"), &["probe"], "Probe input"),
    d("probe.wireless-selected", Bit, IN, "probe_loaded", None, &["probe"], "Probe is in the spindle"),
    d("probe.fault", Bit, IN, "probe_fault", None, &["probe"], "Probe fault"),
    d("probe.toolsetter", Bit, IN, "toolsetter_tripped", None, &["probe"], "Tool setter input"),
    d("probe.blast-request", Bit, OUT, "setter_blast", None, &["probe"], "Tool setter air blast"),
    d("probe.blast-active", Bit, IN, "setter_blast_active", None, &["probe"], "Tool setter air blast is on"),
    // Tool change (the dialog, with or without the power drawbar).
    d("toolchange.tool-change", Bit, IN, "tool_change", Some("iocontrol.0.tool-change"), TC, "M6 asks for a tool change"),
    d("toolchange.tool-number", S32, IN, "tool_number", Some("iocontrol.0.tool-prep-number"), TC, "M6 tool number"),
    d("toolchange.tool-changed", Bit, OUT, "tool_changed", Some("iocontrol.0.tool-changed"), TC, "Tool change done"),
    d("toolchange.spindle-on", Bit, IN, "spindle_on", Some("spindle.0.on"), TC, "Spindle is on"),
    // Power drawbar.
    d("drawbar.extended", Bit, IN, "drawbar_extended", None, &["drawbar"], "Drawbar is told to release"),
    d("drawbar.manual-request", Bit, OUT, "drawbar_request", None, &["drawbar"], "Drawbar release request"),
    d("drawbar.spindle-rearm-required", Bit, IN, "spindle_rearm_required", None, &["drawbar"], "Spindle needs M5 before it starts again"),
    d("drawbar.touch-off-requested", Bit, IN, "touch_off_requested", None, &["drawbar"], "Tool touch-off is asked for"),
    d("drawbar.touch-off-set", Bit, OUT, "touch_off_set", None, &["drawbar"], "Ask for a tool touch-off"),
    d("drawbar.touch-off-reset", Bit, OUT, "touch_off_reset", None, &["drawbar"], "Clear the touch-off request"),
    d("drawbar.nose-reference-valid", Bit, IN, "nose_reference_valid", None, &["drawbar"], "Spindle nose reference is valid"),
    // Why motion is held.
    d("interlock.held", Bit, IN, "motion_held", Some("motion.feed-inhibit"), &["interlock"], "Motion is held"),
    d("interlock.probe-fault", Bit, IN, "probe_fault", None, &["interlock"], "Probe fault"),
    d("interlock.drives-not-ready", Bit, IN, "drives_not_ready", None, &["interlock"], "Drives not ready"),
    // Servo drives, per axis.
    ax("drives.ready-x", "drive_ready_x", 'X', "X drive is ready"),
    ax("drives.ready-y", "drive_ready_y", 'Y', "Y drive is ready"),
    ax("drives.ready-z", "drive_ready_z", 'Z', "Z drive is ready"),
    ax("drives.ready-a", "drive_ready_a", 'A', "A drive is ready"),
    ax("drives.ready-b", "drive_ready_b", 'B', "B drive is ready"),
    ax("drives.ready-c", "drive_ready_c", 'C', "C drive is ready"),
    ax("drives.fault-x", "drive_fault_x", 'X', "X drive fault"),
    ax("drives.fault-y", "drive_fault_y", 'Y', "Y drive fault"),
    ax("drives.fault-z", "drive_fault_z", 'Z', "Z drive fault"),
    ax("drives.fault-a", "drive_fault_a", 'A', "A drive fault"),
    ax("drives.fault-b", "drive_fault_b", 'B', "B drive fault"),
    ax("drives.fault-c", "drive_fault_c", 'C', "C drive fault"),
    // Cutting load.
    d("load.value", Float, IN, "load", None, &["load"], "Cutting load, 0 to 1"),
    // WHB04B pendant (xhc-whb04b-6): the driver's own pins by default.
    d("pendant.connected", Bit, IN, "pendant_connected", Some("whb.pendant.is-connected"), &["pendant"], "Pendant is connected"),
    d("pendant.asleep", Bit, IN, "pendant_asleep", Some("whb.pendant.is-sleeping"), &["pendant"], "Pendant is asleep"),
    d("pendant.start", Bit, IN, "pendant_start", Some("whb.button.start-pause"), &["pendant"], "Pendant START/PAUSE"),
    d("pendant.auto-request", Bit, IN, "pendant_auto", Some("whb.halui.mode.auto"), &["pendant"], "Pendant asks for AUTO"),
    d("pendant.ask-manual", Bit, IN, "pendant_ask_manual", Some("whb.halui.mode.manual"), &["pendant"], "Pendant asks for MANUAL"),
    d("pendant.ask-teleop", Bit, IN, "pendant_ask_teleop", Some("whb.halui.mode.teleop"), &["pendant"], "Pendant asks for teleop"),
    d("pendant.ask-joint", Bit, IN, "pendant_ask_joint", Some("whb.halui.mode.joint"), &["pendant"], "Pendant asks for joint mode"),
    d("pendant.ask-mdi", Bit, IN, "pendant_ask_mdi", Some("whb.halui.mode.mdi"), &["pendant"], "Pendant asks for MDI"),
    d("pendant.mode-manual", Bit, OUT, "pendant_mode_manual", Some("halui.mode.manual"), &["pendant"], "MANUAL request passed to halui"),
    d("pendant.mode-teleop", Bit, OUT, "pendant_mode_teleop", Some("halui.mode.teleop"), &["pendant"], "Teleop request passed to halui"),
    d("pendant.mode-joint", Bit, OUT, "pendant_mode_joint", Some("halui.mode.joint"), &["pendant"], "Joint mode request passed to halui"),
    d("pendant.mode-mdi", Bit, OUT, "pendant_mode_mdi", Some("halui.mode.mdi"), &["pendant"], "MDI request passed to halui"),
    // Panel buttons and knobs: through ferrum's own checks, as the screen's buttons.
    d("panel.cycle-start", Bit, IN, "cycle_start", None, &["panel"], "CYCLE START button"),
    d("panel.feed-hold", Bit, IN, "feed_hold", None, &["panel"], "FEED HOLD button"),
    d("panel.stop", Bit, IN, "stop", None, &["panel"], "STOP button"),
    d("panel.single-block", Bit, IN, "single_block", None, &["panel"], "SINGLE BLOCK switch"),
    d("panel.feed-override-counts", S32, IN, "feed_override_counts", None, &["panel"], "Feed override knob, 1 % per count"),
    d("panel.spindle-override-counts", S32, IN, "spindle_override_counts", None, &["panel"], "Spindle override knob, 1 % per count"),
    d("panel.rapid-override-counts", S32, IN, "rapid_override_counts", None, &["panel"], "Rapid override knob, 1 % per count"),
    d("panel.unlock", Bit, IN, "unlock", None, &["panel"], "Key switch: SETUP may change settings"),
    // An MPG follows MANUAL: the axis last jogged there, and the jog step.
    d("mpg.axis-x", Bit, OUT, "mpg_axis_x", None, &["panel"], "MPG moves X"),
    d("mpg.axis-y", Bit, OUT, "mpg_axis_y", None, &["panel"], "MPG moves Y"),
    d("mpg.axis-z", Bit, OUT, "mpg_axis_z", None, &["panel"], "MPG moves Z"),
    d("mpg.axis-a", Bit, OUT, "mpg_axis_a", None, &["panel"], "MPG moves A"),
    d("mpg.axis-b", Bit, OUT, "mpg_axis_b", None, &["panel"], "MPG moves B"),
    d("mpg.axis-c", Bit, OUT, "mpg_axis_c", None, &["panel"], "MPG moves C"),
    d("mpg.increment", Float, OUT, "mpg_increment", None, &["panel"], "MPG step per count (machine units)"),
    // Stack light.
    d("status.error", Bit, OUT, "light_error", None, &["panel"], "Stack light red: E-stop or a fault"),
    d("status.waiting", Bit, OUT, "light_waiting", None, &["panel"], "Stack light amber: paused or waiting for the operator"),
    d("status.running", Bit, OUT, "light_running", None, &["panel"], "Stack light green: a program runs"),
    // Screen watchdog: wired by the module itself (modules.rs).
    Def { pin: "watchdog.heartbeat", ty: Bit, input: OUT, name: None, auto: None, modules: &["watchdog"], axis: None, title: "Flips once per drawn frame" },
    Def { pin: "watchdog.ok", ty: Bit, input: IN, name: None, auto: None, modules: &["watchdog"], axis: None, title: "The screen drew in the last second" },
];

/// `[hal]` names that are not pins of the table: the screen watchdog's
/// trip input (modules.rs wires it).
pub const WATCHDOG_TRIP: &str = "screen_watchdog_trip";

/// Modules that are always on (not in the module list).
pub const ALWAYS_ON: &[&str] = &["machine", "panel", "watchdog", "setup"];

/// The pins of a machine with these axis letters.
pub fn for_axes(axes: &str) -> impl Iterator<Item = &'static Def> + '_ {
    ALL.iter().filter(move |d| d.axis.is_none_or(|a| axes.contains(a)))
}

/// The defs a module uses that have a `[hal]` name.
pub fn links(module: &str) -> impl Iterator<Item = &'static Def> + '_ {
    ALL.iter().filter(move |d| d.name.is_some() && d.modules.contains(&module))
}

/// Every pin with this `[hal]` name (one name may feed several pins).
pub fn by_name(name: &str) -> impl Iterator<Item = &'static Def> + '_ {
    ALL.iter().filter(move |d| d.name == Some(name))
}

/// Whether `name` is a `[hal]` name ferrum knows.
pub fn known(name: &str) -> bool {
    name == WATCHDOG_TRIP || by_name(name).next().is_some()
}

/// `ferrum --hal-pins`: the table as text.
pub fn table(axes: &str) -> String {
    let mut s = String::from("pin                                   type   dir  [hal] name              default\n");
    for d in for_axes(axes) {
        s.push_str(&format!(
            "ferrum.{:<31} {:<6} {:<4} {:<23} {}\n",
            d.pin,
            d.ty.as_str(),
            if d.input { "IN" } else { "OUT" },
            d.name.unwrap_or("-"),
            d.auto.unwrap_or("-")
        ));
    }
    s.push_str(&format!("{:<44} {:<4} {:<23} {}\n", "(no pin: a bit input of the machine)", "", WATCHDOG_TRIP, "gate iocontrol.0.emc-enable-in"));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_unique_and_names_agree() {
        for (i, a) in ALL.iter().enumerate() {
            assert!(ALL[i + 1..].iter().all(|b| b.pin != a.pin), "{} twice", a.pin);
            // A name shared by pins means one signal feeding them all: same
            // type and direction.
            if let Some(n) = a.name {
                for b in by_name(n) {
                    assert_eq!((a.ty, a.input), (b.ty, b.input), "{n}");
                }
            }
        }
    }

    #[test]
    fn axes_filter_drive_pins() {
        let pins: Vec<_> = for_axes("XYZ").filter(|d| d.pin.starts_with("drives.")).map(|d| d.pin).collect();
        assert_eq!(pins, ["drives.ready-x", "drives.ready-y", "drives.ready-z", "drives.fault-x", "drives.fault-y", "drives.fault-z"]);
        assert!(table("XYZ").contains("ferrum.probe.tripped"));
    }
}
