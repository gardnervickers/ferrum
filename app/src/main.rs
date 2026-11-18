//! millgui: the mill's LinuxCNC display program.
//!
//! LinuxCNC starts it as `millgui -ini <file>`. `--sim` runs against the
//! simulated machine instead (the default on builds without NML, e.g. the Mac).

mod calc;
mod config;
mod conv;
mod files;
mod headless;
mod history;
mod job;
mod machine_info;
mod checks;
mod halmap;
mod machine_cfg;
mod manual;
mod mdi;
mod modules;
mod picker;
mod plot;
mod powerfeed;
mod program;
mod remote;
mod restart;
mod renderer;
mod settings;
mod setup;
mod textentry;
mod toolchange;
mod offsets;
mod probe;
mod probe_anim;
mod tools;
mod util;
mod models;

use config::Config;
use machine::{Command, Event, Handle, Interp, JointSel, MessageKind, SpindleDirection, Status, TaskState, AXIS_LETTERS};
use modules::{Module, Pill};
use slint::{ComponentHandle, SharedString};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command as Proc;
use std::rc::Rc;
use std::time::{Duration, Instant};

slint::include_modules!();

struct Args {
    ini: Option<PathBuf>,
    sim: bool,
    windowed: bool,
    screenshot: Option<PathBuf>,
    screen: Option<String>,
    /// Screenshot extras: "keypad", "keys:<value>:<typed>[=]", "messages", "mdi".
    show: Vec<String>,
    /// Simulator: program to load at start.
    open: Option<PathBuf>,
    /// Screenshot window size in pixels (default 1920x1080).
    size: Option<(u32, u32)>,
    /// `--check [INI]`: print the configuration check and exit.
    check: bool,
    /// `--check --write-ini`: also add the INI lines it asks for.
    write_ini: bool,
}

fn parse_args() -> Args {
    let mut a = Args { ini: std::env::var_os("INI_FILE_NAME").map(PathBuf::from), sim: false, windowed: false, screenshot: None, screen: None, show: Vec::new(), open: None, size: None, check: false, write_ini: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-ini" | "--ini" => a.ini = it.next().map(PathBuf::from),
            "--sim" => a.sim = true,
            "--windowed" => a.windowed = true,
            "--screenshot" => {
                a.screenshot = it.next().map(PathBuf::from);
                a.sim = true;
            }
            "--screen" => a.screen = it.next(),
            "--show" => a.show.extend(it.next()),
            "--open" => a.open = it.next().map(PathBuf::from),
            "--size" => a.size = it.next().and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))),
            "--check" => a.check = true,
            "--write-ini" => {
                a.check = true;
                a.write_ini = true;
            }
            other if a.check && !other.starts_with('-') => a.ini = Some(PathBuf::from(other)),
            _ => {}
        }
    }
    if !cfg!(target_os = "linux") && !a.check {
        a.sim = true;
    }
    a
}

/// State the UI thread keeps between status updates.
#[derive(Default)]
struct UiState {
    status: Option<Status>,
    connected: bool,
    /// Set by Disconnected; the next status announces the reconnection.
    lost: bool,
    cycle_started: Option<Instant>,
    cycle_elapsed: Duration,
}

fn connect_machine(args: &Args, cfg: &Config) -> Box<dyn machine::Machine> {
    if args.sim {
        let mut sim = machine::Sim::new();
        // `--open <file>` (simulator only): start powered, homed, with a program loaded.
        if let Some(f) = &args.open {
            use machine::Machine;
            for c in [Command::SetState(TaskState::EstopReset), Command::SetState(TaskState::On), Command::Home(JointSel::All), Command::ProgramOpen(f.clone())] {
                let _ = sim.send(&c);
            }
        }
        return Box::new(sim);
    }
    let nml = cfg.get("EMC", "NML_FILE").map(|f| cfg.dir().join(f).to_string_lossy().into_owned());
    // LinuxCNC starts the display after task is up, but give it a moment.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match machine::connect(nml.as_deref()) {
            Ok(m) => return m,
            Err(e) if Instant::now() > deadline => {
                eprintln!("millgui: {e}");
                std::process::exit(1);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// Create module pins, mark the component ready, then net them with each
/// enabled module's `millgui/hal/<id>.hal` and the optional
/// [MILLGUI]POSTGUI_HALFILE. Returns the component, which must live as long
/// as the GUI (dropping it calls hal_exit).
fn setup_hal(cfg: &Config, modules: &mut [Box<dyn Module>], notes: &mut Vec<String>) -> Option<lcnc::hal::Component> {
    let comp = match lcnc::hal::Component::new("millgui") {
        Ok(c) => c,
        Err(e) => {
            notes.push(format!("HAL unavailable: {e}"));
            return None;
        }
    };
    for m in modules.iter_mut() {
        if let Err(e) = m.create_pins(&comp) {
            notes.push(format!("{}: {e}", m.id()));
        }
    }
    if let Err(e) = comp.ready() {
        notes.push(e.to_string());
        return Some(comp);
    }
    let Some(ini) = cfg.path.as_ref() else { return Some(comp) };
    // A module's own millgui/hal/<id>.hal (the mymill profile), else its
    // pins connect through [hal] in millgui.toml or their default targets.
    let mc = machine_cfg::get();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut known: Vec<&str> = Vec::new();
    for m in modules.iter_mut() {
        let file = PathBuf::from(format!("millgui/hal/{}.hal", m.id()));
        let has_file = cfg.dir().join(&file).is_file();
        if has_file {
            files.push(file);
        }
        for l in m.hal_links() {
            known.push(l.name);
            let target = mc.hal.get(l.name).map(String::as_str).or(if has_file { None } else { l.auto });
            match target {
                Some(t) => match halmap::link(l.pin, l.input, t) {
                    Ok(done) => {
                        eprintln!("millgui: HAL {done}");
                        m.linked(l.name);
                    }
                    Err(e) => notes.push(format!("[hal]{}: {e}", l.name)),
                },
                None if has_file => m.linked(l.name),
                None => {}
            }
        }
    }
    for k in mc.hal.keys() {
        if !known.contains(&k.as_str()) {
            notes.push(format!("[hal]{k}: no enabled module uses this name"));
        }
    }
    files.extend(mc.postgui(cfg).into_iter().map(PathBuf::from));
    for f in files {
        if let Err(e) = halcmd_file(&cfg.dir(), ini, &f) {
            notes.push(e);
        }
    }
    Some(comp)
}

fn halcmd_file(dir: &Path, ini: &Path, file: &Path) -> Result<(), String> {
    // -k: keep going past a failed line, so one missing pin (e.g. the Pi's
    // config is older than this file) does not drop the module's other nets.
    let out = Proc::new("halcmd")
        .current_dir(dir)
        .arg("-k")
        .arg("-i")
        .arg(ini)
        .arg("-f")
        .arg(file)
        .output()
        .map_err(|e| format!("halcmd: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let errs = String::from_utf8_lossy(&out.stderr).into_owned();
    eprintln!("halcmd -f {}:\n{errs}", file.display());
    Err(halcmd_summary(file, &errs))
}

/// "HAL drawbar.hal: line 4: Pin 'x' does not exist (+1 more)" from
/// halcmd's stderr ("<file>:<line>: <reason>" per failed line).
fn halcmd_summary(file: &Path, stderr: &str) -> String {
    let name = file.file_name().map_or_else(|| file.display().to_string(), |n| n.to_string_lossy().into_owned());
    let fails: Vec<String> = stderr
        .lines()
        .filter_map(|l| {
            let l = l.trim().trim_start_matches("HAL:").trim();
            let (_, rest) = l.split_once(".hal:")?;
            let (line, why) = rest.split_once(':')?;
            Some(format!("line {}: {}", line.trim(), why.trim().trim_start_matches("ERROR:").trim()))
        })
        .collect();
    match fails.as_slice() {
        [] => format!("HAL {name}: halcmd failed: {}", stderr.lines().last().unwrap_or("").trim()),
        [one] => format!("HAL {name}: {one} (the rest connected)"),
        [first, rest @ ..] => format!("HAL {name}: {first} (+{} more; the rest connected)", rest.len()),
    }
}

#[cfg(test)]
mod hal_tests {
    use super::*;

    #[test]
    fn summary_names_the_failed_lines() {
        let e = "millgui/hal/drawbar.hal:4: Pin 'spindle-rearm-required-pin' does not exist\nmillgui/hal/drawbar.hal:9: Pin 'x.set' does not exist\n";
        assert_eq!(
            halcmd_summary(Path::new("millgui/hal/drawbar.hal"), e),
            "HAL drawbar.hal: line 4: Pin 'spindle-rearm-required-pin' does not exist (+1 more; the rest connected)"
        );
    }
}

/// A DRO number: degrees to 2 places, lengths to the machine units' places.
fn fmt_axis(v: f64, letter: char) -> String {
    if matches!(letter, 'A' | 'B' | 'C') {
        format!("{v:.2}°")
    } else {
        machine_info::get().units.fmt(v)
    }
}

/// AXIS_LETTERS indices of the machine's axes, in [TRAJ]COORDINATES order.
fn axis_indices() -> Vec<usize> {
    machine_info::get().axes.iter().map(|a| a.index).collect()
}

/// Mode and homing pills. A rotary without a home switch is "referenced"
/// where it stands, so its pill says REF rather than implying it is homed.
fn core_pills_front(s: &Status, axes: &[usize], out: &mut Vec<Pill>) {
    out.push(if s.estop() {
        Pill::new("E-STOP", "bad")
    } else if !s.power_on() {
        Pill::new("OFF", "warn")
    } else {
        match s.mode {
            machine::Mode::Manual => Pill::new("MANUAL", "info"),
            machine::Mode::Auto if s.interp == Interp::Paused => Pill::new("PAUSED", "warn"),
            machine::Mode::Auto => Pill::new("AUTO", "good"),
            machine::Mode::Mdi => Pill::new("MDI", "info"),
            machine::Mode::Unknown(_) => Pill::new("MODE?", "warn"),
        }
    });
    let mi = machine_info::get();
    let homed = |i: usize| mi.axis_homed(s, i);
    // Axes homed against a switch, and rotaries referenced where they stand.
    let switched = |i: usize| mi.axis(i).is_none_or(|a| a.home_switch || !a.rotary);
    let main: Vec<usize> = axes.iter().copied().filter(|&i| switched(i)).collect();
    let referenced: Vec<usize> = axes.iter().copied().filter(|&i| !switched(i)).collect();
    if !main.iter().all(|&i| homed(i)) {
        out.push(Pill::new("NOT HOMED", "bad").hint(if mi.per_axis_home() { "Hold HOME ALL, or HOME beside an axis." } else { "Hold HOME ALL." }));
        return;
    }
    let mut text = format!("HOMED {}", main.iter().map(|&i| AXIS_LETTERS[i]).collect::<String>());
    let mut kind = "good";
    for &i in &referenced {
        if homed(i) {
            text.push_str(&format!(" · {} REF", AXIS_LETTERS[i]));
        } else {
            text.push_str(&format!(" · {} NO REF", AXIS_LETTERS[i]));
            kind = "warn";
        }
    }
    out.push(Pill::new(text, kind).hint(if kind == "warn" { "Home the rotary axis to reference it where it stands." } else { "" }));
}

/// Commanded spindle state (there is no speed feedback to show).
fn core_pills_back(s: &Status, out: &mut Vec<Pill>) {
    out.push(if s.spindle_direction.sign() == 0.0 || s.spindle_speed == 0.0 {
        Pill::new("SPINDLE CMD OFF", "")
    } else {
        Pill::new(
            format!(
                "SPINDLE CMD {} {:.0}",
                if s.spindle_direction == SpindleDirection::Forward { "CW" } else { "CCW" },
                s.spindle_speed.abs() * s.spindle_scale
            ),
            "info",
        )
    });
}

fn refresh_pills(ui: &MainWindow, st: &UiState, modules: &[Box<dyn Module>], axes: &[usize]) {
    let mut pills = Vec::new();
    if let Some(s) = &st.status {
        core_pills_front(s, axes, &mut pills);
    } else {
        pills.push(Pill::new("NO CONNECTION", "bad"));
    }
    for m in modules {
        m.pills(st.status.as_ref(), &mut pills);
    }
    if let Some(s) = &st.status {
        core_pills_back(s, &mut pills);
    }
    let live = ui.global::<Live>();
    let mut pills = pills.into_iter();
    // The first is the mode; the rest are the status rows.
    let mode = pills.next().unwrap_or_else(|| Pill::new("", ""));
    let rows: Vec<Pill> = pills.collect();
    let (text, kind, flash) = status_summary(&rows);
    live.set_mode_pill(PillData { text: mode.text.into(), kind: mode.kind.into(), hint: "".into() });
    live.set_status_text(text.into());
    live.set_status_kind(kind.into());
    live.set_status_flash(flash);
    let model: Vec<PillData> = rows.into_iter().map(|p| PillData { text: p.text.into(), kind: p.kind.into(), hint: p.hint.into() }).collect();
    crate::models::sync(live.get_pills(), model, |m| live.set_pills(m));
}

/// The one status pill: ALL OK, or the worst row (faults before warnings)
/// and how many more there are. A fault makes it flash.
fn status_summary(rows: &[Pill]) -> (String, &'static str, bool) {
    let mut wrong: Vec<&Pill> = rows.iter().filter(|p| p.kind == "bad").collect();
    let flash = !wrong.is_empty();
    wrong.extend(rows.iter().filter(|p| p.kind == "warn"));
    match wrong.first() {
        None => ("ALL OK".into(), "good", false),
        Some(first) if wrong.len() == 1 => (first.text.clone(), first.kind, flash),
        Some(first) => (format!("{} +{}", first.text, wrong.len() - 1), first.kind, flash),
    }
}

/// The tool tip in work coordinates, the frame the interpreter preview uses.
/// The backplot's table and tool: the loaded tool's diameter and length
/// from the tool table (the applied G43 length if the table has none), and
/// the work offset.
fn machine_pose(_ui: &MainWindow, s: &Status) -> preview::machine::Pose {
    let (dia, len) = tools::loaded_tool().unwrap_or((0.0, 0.0));
    let len = if len > 0.5 { len } else { s.tool_offset[2] };
    preview::machine::Pose { tip: tool_tip(s), tool_len: len, tool_dia: dia, offset: [s.g5x_offset[0], s.g5x_offset[1], s.g5x_offset[2]] }
}

fn tool_tip(s: &Status) -> [f64; 4] {
    let w = s.work_position();
    [w[0], w[1], w[2], w[3]]
}

fn apply_status(ui: &MainWindow, st: &mut UiState, s: &Status, axes: &[usize]) {
    let live = ui.global::<Live>();
    let work = s.work_position();
    let mi = machine_info::get();
    let rows: Vec<DroRow> = axes.iter().map(|&i| DroRow { axis: AXIS_LETTERS[i].to_string().into(), homed: mi.axis_homed(s, i) }).collect();
    let pos: Vec<DroPos> = axes
        .iter()
        .map(|&i| {
            let l = AXIS_LETTERS[i];
            DroPos {
                work: fmt_axis(work[i], l).into(),
                mach: fmt_axis(s.actual[i], l).trim_end_matches('°').into(),
                dtg: fmt_axis(s.dtg[i], l).trim_end_matches('°').into(),
            }
        })
        .collect();
    crate::models::sync(live.get_dro(), rows, |m| live.set_dro(m));
    crate::models::sync(live.get_dro_pos(), pos, |m| live.set_dro_pos(m));
    live.set_estop(s.estop());
    live.set_power(s.power_on());
    live.set_homed(axes.iter().all(|&i| mi.axis_homed(s, i)));
    live.set_wcs(s.wcs.name().into());
    live.set_tool(format!("T{}", s.tool_in_spindle).into());
    live.set_tc_from(format!("T{}", s.tool_in_spindle).into());
    live.set_feed(format!("{:.0}", s.feed_per_min()).into());
    let rpm = s.spindle_speed * s.spindle_scale * s.spindle_direction.sign();
    live.set_rpm(format!("{:.0}", rpm.abs()).into());
    live.set_modal(s.modal_string().into());
    live.set_feed_pct((s.feed_scale * 100.0).round() as i32);
    live.set_rapid_pct((s.rapid_scale * 100.0).round() as i32);
    live.set_spindle_pct((s.spindle_scale * 100.0).round() as i32);
    // In MDI, the file is a subroutine the command called (the M6 remap,
    // a probe macro), not the loaded program.
    let mdi = s.mode == machine::Mode::Mdi;
    if !mdi {
        live.set_program(
            Path::new(&s.file).file_name().map(|f| f.to_string_lossy().to_uppercase()).unwrap_or_default().into(),
        );
    }
    // LinuxCNC keeps the last run's lines after it ends; idle shows none.
    // The line motion is executing; current_line runs ahead with the
    // interpreter's read-ahead (to the next M6, say) while moves queue.
    let line = if s.motion_line > 0 { s.motion_line } else { s.current_line };
    live.set_line(if s.interp == Interp::Idle || mdi { 0 } else { line });
    live.set_running(s.busy());
    live.set_paused(s.interp == Interp::Paused);
    live.set_optional_stop(s.optional_stop);
    live.set_block_delete(s.block_delete);
    let ready = s.power_on() && s.all_homed();
    live.set_can_start(ready && (s.interp == Interp::Paused || (s.interp == Interp::Idle && !s.file.is_empty())));

    // Cycle timer: runs while a program is running in auto mode.
    let running = s.mode == machine::Mode::Auto && s.busy();
    let was = st.status.as_ref().is_some_and(|p| p.mode == machine::Mode::Auto && p.busy());
    if running && !was {
        st.cycle_started = Some(Instant::now());
        st.cycle_elapsed = Duration::ZERO;
    } else if !running && was {
        if let Some(t) = st.cycle_started.take() {
            st.cycle_elapsed = t.elapsed();
        }
    }
    st.status = Some(s.clone());
}

fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn wire_actions(ui: &MainWindow, machine: &Handle, state: &Rc<RefCell<UiState>>, cfg: &Config) {
    let actions = ui.global::<Actions>();
    let feed_max = cfg.get_f64("DISPLAY", "MAX_FEED_OVERRIDE").unwrap_or(2.0);
    let spindle_max = cfg.get_f64("DISPLAY", "MAX_SPINDLE_OVERRIDE").unwrap_or(2.0);
    let spindle_min = cfg.get_f64("DISPLAY", "MIN_SPINDLE_OVERRIDE").unwrap_or(0.5);
    let live = ui.global::<Live>();
    live.set_feed_max((feed_max * 100.0).round() as i32);
    live.set_spindle_max((spindle_max * 100.0).round() as i32);
    live.set_spindle_min((spindle_min * 100.0).round() as i32);

    let m = machine.clone();
    actions.on_estop_trip(move || m.send(Command::SetState(TaskState::Estop)));
    let m = machine.clone();
    actions.on_estop_reset(move || m.send(Command::SetState(TaskState::EstopReset)));
    let m = machine.clone();
    actions.on_home_all(move || m.send(Command::Home(JointSel::All)));
    // Every joint behind the axis (both sides of a gantry), one at a time:
    // LinuxCNC drops a home command that comes while another joint homes.
    let (m, st, timer) = (machine.clone(), state.clone(), Rc::new(slint::Timer::default()));
    actions.on_home_axis(move |axis| {
        let mut joints = axis.chars().next().map(|l| machine_info::get().home_joints(l)).unwrap_or_default();
        if joints.is_empty() {
            return;
        }
        let mut last = joints.remove(0);
        m.send(Command::Home(JointSel::Joint(last as u8)));
        if joints.is_empty() {
            return;
        }
        let (m, st, weak) = (m.clone(), st.clone(), Rc::downgrade(&timer));
        let (mut sent, start) = (Instant::now(), Instant::now());
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
            let stop = || weak.upgrade().inspect(|t| t.stop()).map(|_| ());
            let st = st.borrow();
            let Some(s) = st.status.as_ref() else { return };
            let homing = s.homing.iter().any(|h| *h);
            let done = s.homed.get(last).copied().unwrap_or(false);
            if start.elapsed() > Duration::from_secs(180) || (!homing && !done && sent.elapsed() > Duration::from_secs(3)) {
                // Homing stopped without this joint homed: leave the rest.
                stop();
                return;
            }
            if homing || !done {
                return;
            }
            last = joints.remove(0);
            m.send(Command::Home(JointSel::Joint(last as u8)));
            sent = Instant::now();
            if joints.is_empty() {
                stop();
            }
        });
    });
    let (m, st) = (machine.clone(), state.clone());
    actions.on_power(move || {
        let on = st.borrow().status.as_ref().is_some_and(|s| s.power_on());
        m.send(Command::SetState(if on { TaskState::Off } else { TaskState::On }));
    });
    let (m, st) = (machine.clone(), state.clone());
    actions.on_override(move |which, delta| {
        let Some(s) = st.borrow().status.clone() else { return };
        let step = delta as f64 / 100.0;
        let cmd = match which.as_str() {
            "feed" => Command::FeedScale((s.feed_scale + step).clamp(0.0, feed_max)),
            "rapid" => Command::RapidScale((s.rapid_scale + step).clamp(0.0, 1.0)),
            "spindle" => Command::SpindleScale((s.spindle_scale + step).clamp(spindle_min, spindle_max)),
            _ => return,
        };
        m.send(cmd);
    });
    let m = machine.clone();
    actions.on_override_reset(move |which| {
        m.send(match which.as_str() {
            "feed" => Command::FeedScale(1.0),
            "rapid" => Command::RapidScale(1.0),
            _ => Command::SpindleScale(1.0),
        });
    });
    let (m, st, weak) = (machine.clone(), state.clone(), ui.as_weak());
    let probe_tool = machine_cfg::get().probe_tool(&cfg);
    let w = ui.as_weak();
    actions.on_toast_dismiss(move || {
        if let Some(ui) = w.upgrade() {
            ui.global::<Live>().set_toast("".into());
        }
    });
    actions.on_cycle_start(move || {
        let Some(s) = st.borrow().status.clone() else { return };
        let Some(ui) = weak.upgrade() else { return };
        let single = ui.global::<Live>().get_single_block();
        if s.interp == Interp::Idle {
            // Probe Basic's checks before a program starts, then the JOB
            // page's rules for job programs (job_session.cycle_start_problem).
            let block = ui.global::<Live>().get_motion_block();
            // A job with probing ops loads the probe and puts it away itself.
            let probing = gcode_lines(&ui).iter().any(|l| l.contains("(MYMILL-PROBE"));
            let problem = if s.tool_in_spindle == probe_tool && !probing {
                Some("Unload the probe before cycle start".to_string())
            } else if !block.is_empty() {
                Some(block.to_string())
            } else if let Some(p) = plot::with_ret(|p| plot::start_problem(p, 0)).flatten() {
                Some(p)
            } else {
                JOB.with(|j| j.borrow().clone()).and_then(|j| j.borrow_mut().cycle_start_problem(0))
            };
            if let Some(p) = problem {
                let live = ui.global::<Live>();
                live.set_message(p.clone().into());
                live.set_message_error(true);
                toast(&ui, format!("CYCLE START refused: {p}"));
                return;
            }
            if probing {
                with_probe(|p| p.job_started());
            }
        }
        match (s.interp, single) {
            (Interp::Paused, true) => m.send(Command::Step),
            (Interp::Paused, false) => m.send(Command::Resume),
            // The worker switches to AUTO first (and says so if it can't).
            (Interp::Idle, true) => m.send(Command::Step),
            (Interp::Idle, false) => m.send(Command::ProgramRun(0)),
            _ => {}
        }
    });
    let (m, st) = (machine.clone(), state.clone());
    // Only a running program or MDI command can be held. Task takes a pause
    // while idle too and then reports PAUSED, which blocks MDI and mode
    // changes until CYCLE START resumes nothing (the button is greyed out
    // when idle; this covers the remote screen).
    actions.on_feed_hold(move || {
        if st.borrow().status.as_ref().is_some_and(|s| s.busy() && s.interp != Interp::Paused) {
            m.send(Command::Pause);
        }
    });
    let m = machine.clone();
    // A restart under way loads the original program again once the abort
    // leaves the interpreter idle.
    actions.on_stop(move || m.send(Command::Abort));
    let (m, st, weak) = (machine.clone(), state.clone(), ui.as_weak());
    actions.on_gcode_run_from_selected(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some(s) = st.borrow().status.clone() else { return };
        let live = ui.global::<Live>();
        let sel = live.get_gcode_selected();
        if sel < 0 {
            return;
        }
        let plan = restart::plan(&gcode_lines(&ui), sel as usize);
        let block = live.get_motion_block();
        let problem = if s.tool_in_spindle == probe_tool && !plan.changes_tool {
            Some("Unload the probe first".to_string())
        } else if !block.is_empty() {
            Some(block.to_string())
        } else if let Some(p) = plan.refuse(&s) {
            Some(p)
        } else if let Some(p) = plot::with_ret(|p| plot::start_problem(p, sel as u32 + 1)).flatten() {
            Some(p)
        } else {
            JOB.with(|j| j.borrow().clone()).and_then(|j| j.borrow_mut().cycle_start_problem(sel + 1))
        };
        if let Some(p) = problem {
            live.set_message(p.clone().into());
            live.set_message_error(true);
            toast(&ui, format!("RUN FROM SELECTED refused: {p}"));
            return;
        }
        if gcode_lines(&ui).iter().any(|l| l.contains("(MYMILL-PROBE")) {
            with_probe(|p| p.job_started());
        }
        let mut run = match restart::Restart::new(&plan, &gcode_lines(&ui), &s.file) {
            Ok(run) => run,
            Err(e) => {
                live.set_message(e.clone().into());
                live.set_message_error(true);
                toast(&ui, e);
                return;
            }
        };
        RESTART_COPY.with(|c| *c.borrow_mut() = Some((run.copy.clone(), run.original.clone())));
        PROGRAM_FILE.with(|p| p.borrow_mut().0.clear());
        if run.pump(&s, &m) {
            RESTART.with(|r| *r.borrow_mut() = Some(run));
        }
    });
    let weak = ui.as_weak();
    actions.on_plot_check_tap(move || {
        let Some(ui) = weak.upgrade() else { return };
        let Some((text, line)) = plot::with_ret(|p| plot::check_details(p)).flatten() else { return };
        let live = ui.global::<Live>();
        if line > 0 {
            live.set_run_tab("program".into());
            live.set_gcode_selected(line as i32 - 1);
        }
        live.set_message(text.into());
        live.set_message_error(live.get_plot_check_level() == 2);
    });
    let weak = ui.as_weak();
    actions.on_single_block(move || {
        if let Some(ui) = weak.upgrade() {
            let live = ui.global::<Live>();
            live.set_single_block(!live.get_single_block());
        }
    });
    let (m, st) = (machine.clone(), state.clone());
    actions.on_optional_stop(move || {
        let on = st.borrow().status.as_ref().is_some_and(|s| s.optional_stop);
        m.send(Command::OptionalStop(!on));
    });
    let (m, st) = (machine.clone(), state.clone());
    actions.on_block_delete(move || {
        let on = st.borrow().status.as_ref().is_some_and(|s| s.block_delete);
        m.send(Command::BlockDelete(!on));
    });
    // LinuxCNC shuts the whole session down when its display program exits.
    actions.on_exit(|| {
        let _ = slint::quit_event_loop();
    });
    let weak = ui.as_weak();
    actions.on_navigate(move |id| {
        if let Some(ui) = weak.upgrade() {
            // Overlays belong to the screen that opened them: a keypad left
            // open would write to whatever that screen has selected later.
            let live = ui.global::<Live>();
            if live.get_keypad_open() {
                ui.global::<Actions>().invoke_keypad_done(false);
            }
            if live.get_text_open() {
                ui.global::<Actions>().invoke_text_done(false);
            }
            if live.get_pick_open() {
                ui.global::<Actions>().invoke_pick(-1);
            }
            // CONV and JOB write programs; show them without a REFRESH.
            if id == "files" {
                ui.global::<Actions>().invoke_files_refresh();
            }
            live.set_screen(id);
        }
    });
}

fn main() -> Result<(), slint::PlatformError> {
    let args = parse_args();
    let mut notes = Vec::new();
    let cfg = match &args.ini {
        Some(p) => Config::load(p).unwrap_or_else(|e| {
            notes.push(e);
            Config::default()
        }),
        None => Config::default(),
    };
    let mc = machine_cfg::init(&cfg, &mut notes);
    let mi = machine_info::init(&cfg, mc);
    jobcore::mill_ops::set_machine_top(&mi.units.gcode(mi.z_top));
    machine::set_axis_joints(std::array::from_fn(|i| mi.jog_joint(i).map(|j| j as u8)));
    if let Some(u) = &mi.units_unknown {
        notes.push(format!("[TRAJ]LINEAR_UNITS = {u} is not a unit millgui knows; showing millimetres"));
    }
    let hal: Box<dyn checks::Hal> = if args.sim { Box::new(checks::NoHal) } else { Box::new(checks::Halcmd::new()) };
    let (mut modules, problems) = modules::build(&cfg, hal.as_ref());
    notes.extend(problems);

    if args.check {
        let ids: Vec<String> = modules.iter().map(|m| m.id().to_string()).collect();
        let rows = checks::run(&checks::Inputs { cfg: &cfg, mc, mi, notes: &notes, modules: &ids, hal: hal.as_ref() });
        print!("{}", checks::report(&rows));
        if args.write_ini {
            write_ini(&cfg, &rows);
        }
        std::process::exit(if checks::blocked(&rows, checks::Feature::Core).is_some() { 1 } else { 0 });
    }

    if let Some(out) = &args.screenshot {
        return screenshot(&args, &cfg, &modules, out);
    }

    let machine = connect_machine(&args, &cfg);
    // Keep the HAL component alive until exit.
    let _hal = if args.sim { None } else { setup_hal(&cfg, &mut modules, &mut notes) };

    let (choice, why) = renderer::choose(&cfg);
    let renderer = renderer::install(choice);
    eprintln!("millgui: drawing with {why}");
    let ui = MainWindow::new()?;
    if !args.windowed && !args.sim {
        ui.window().set_fullscreen(true);
    }
    // A desktop window may be any size: scale the 1920x1080 layout to fit.
    if args.windowed || args.sim {
        fit_desktop_window(&ui);
    }
    set_nav(&ui, &modules, args.screen.as_deref());
    let live = ui.global::<Live>();
    if !notes.is_empty() {
        for n in &notes {
            eprintln!("millgui: {n}");
        }
        live.set_message(notes.join("; ").into());
        live.set_message_error(true);
    } else if args.sim {
        live.set_message("Simulated machine".into());
    }

    let axes = axis_indices();
    let state = Rc::new(RefCell::new(UiState::default()));
    let modules = Rc::new(modules);

    let weak = ui.as_weak();
    let handle = machine::spawn(machine, Duration::from_millis(50), move |ev| {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                UI_EVENT.with(|f| {
                    if let Some(f) = f.borrow_mut().as_mut() {
                        f(&ui, ev)
                    }
                });
            }
        });
    });

    {
        let state = state.clone();
        let axes = axes.clone();
        let sub_dirs = subroutine_dirs(&cfg);
        UI_EVENT.with(|f| {
            *f.borrow_mut() = Some(Box::new(move |ui: &MainWindow, ev: Event| {
                let live = ui.global::<Live>();
                match ev {
                    Event::Status(mut s) => {
                        program_file(&mut s, &sub_dirs);
                        let mut st = state.borrow_mut();
                        if st.lost {
                            st.lost = false;
                            live.set_message("Reconnected to LinuxCNC".into());
                            live.set_message_error(false);
                        }
                        st.connected = true;
                        live.set_connected(true);
                        apply_status(ui, &mut st, &s, &axes);
                        // A program that pauses right after a (MSG, ...) is
                        // asking for something: the top bar cuts the text
                        // off, so show it in full until the program goes on.
                        let was = WAS_PAUSED.with(|p| p.replace(s.paused));
                        if s.paused && !was {
                            let said = LAST_MSG.with(|l| l.borrow_mut().take());
                            if let Some((text, at)) = said {
                                if at.elapsed() < Duration::from_secs(3) {
                                    toast_until_tapped(ui, text);
                                    live.set_toast_info(true);
                                }
                            }
                        } else if was && !s.paused && live.get_toast_info() {
                            live.set_toast("".into());
                        }
                        with_manual(|mm| manual::apply_status(ui, mm, &s));
                        // An MDI command that calls a subroutine (the M6
                        // remap, probing) shows that file as the open one
                        // while it runs; the loaded program is unchanged.
                        let mdi_sub = s.mode == machine::Mode::Mdi;
                        if !mdi_sub {
                            program::sync(ui, &s.file, s.busy());
                        }
                        SIM_TC.with(|t| {
                            if let Some(t) = t.borrow().as_ref() {
                                t.status(&s);
                            }
                        });
                        if !mdi_sub {
                            plot::sync(ui, &s.file, s.busy());
                        }
                        // After the tools, which know the loaded tool's size.
                        TOOLS.with(|t| {
                            if let Some(t) = t.borrow().as_ref() {
                                tools::apply_status(ui, t, &s);
                            }
                        });
                        plot::with(|p| {
                            plot::tool_moved_homed(p, tool_tip(&s), s.all_homed());
                            plot::machine_pose(p, machine_pose(ui, &s));
                        });
                        OFFSETS.with(|o| {
                            if let Some(o) = o.borrow().as_ref() {
                                offsets::apply_status(ui, o, &s);
                                let live = offsets::preflight_live(o, &s);
                                plot::with(|p| plot::set_live(ui, p, live));
                            }
                        });
                        PROBE.with(|p| {
                            if let Some(p) = p.borrow().as_ref() {
                                probe::apply_status(ui, p, &s);
                            }
                        });
                        UTIL.with(|u| {
                            if let Some(u) = u.borrow().as_ref() {
                                util::apply_status(u, &s);
                            }
                        });
                        JOB.with(|j| {
                            if let Some(j) = j.borrow().as_ref() {
                                job::apply_status(j, &s);
                            }
                        });
                        SETUP.with(|t| {
                            if let Some(t) = t.borrow().as_ref() {
                                setup::apply_status(ui, t, &s);
                            }
                        });
                        // The backplot follows the tool in this same frame.
                        plot::with(|p| plot::redraw_due(ui, p, Duration::from_millis(15)));
                    }
                    Event::Message(m) => {
                        if std::env::var_os("MILLGUI_LOG_MESSAGES").is_some() {
                            eprintln!("millgui: message {:?}: {:?}", m.kind, m.text);
                        }
                        // Macros report results as DEBUG lines.
                        if UTIL.with(|u| u.borrow().as_ref().is_some_and(|u| util::message(u, &m))) {
                            return;
                        }
                        // A job's check reading (MYMILL-MEASURE): kept on the JOB page.
                        if JOB.with(|j| j.borrow().as_ref().is_some_and(|j| j.borrow_mut().message(&m.text))) {
                            return;
                        }
                        if PROBE.with(|p| p.borrow().as_ref().is_some_and(|p| probe::message(ui, p, &m))) {
                            return;
                        }
                        with_manual(|mm| manual::log_message(ui, mm, &m));
                        live.set_message(SharedString::from(m.text.trim()));
                        live.set_message_error(m.kind == MessageKind::Error);
                        // The top bar cuts long text off; an error also
                        // shows in full until it is tapped away.
                        if m.kind == MessageKind::Error {
                            toast_until_tapped(ui, explain_error(m.text.trim()));
                        } else if m.kind == MessageKind::Display {
                            // Shown in full if the program pauses on it; the
                            // pause can be seen before or after the text.
                            if WAS_PAUSED.with(|p| p.get()) {
                                toast_until_tapped(ui, m.text.trim().to_string());
                                live.set_toast_info(true);
                            } else {
                                LAST_MSG.with(|l| *l.borrow_mut() = Some((m.text.trim().to_string(), Instant::now())));
                            }
                        }
                    }
                    Event::Disconnected(e) => {
                        let mut st = state.borrow_mut();
                        st.connected = false;
                        st.lost = true;
                        st.status = None;
                        live.set_connected(false);
                        live.set_message(e.into());
                        live.set_message_error(true);
                    }
                }
            }));
        });
    }

    wire_actions(&ui, &handle, &state, &cfg);
    let manual = manual::wire(&ui, &handle, &cfg, settings::Settings::load(), axes.contains(&3));
    // The configuration check switches off what this machine cannot run,
    // with the reason on the screen (the simulator has no machine to check).
    let ids: Vec<String> = modules.iter().map(|m| m.id().to_string()).collect();
    let check_rows = checks::run(&checks::Inputs { cfg: &cfg, mc: machine_cfg::get(), mi: machine_info::get(), notes: &notes, modules: &ids, hal: hal.as_ref() });
    if !args.sim {
        let rows = &check_rows;
        let live = ui.global::<Live>();
        // Coolant buttons only for the outputs that are wired, unless
        // [buttons] says otherwise.
        let mc = machine_cfg::get();
        let unwired = |key: &str, pin: &str| !mc.is_set("buttons", key) && hal.pin_signal(pin) == Some(Some(String::new()));
        if unwired("mist", "iocontrol.0.coolant-mist") {
            live.set_quick_mist(false);
        }
        if unwired("flood", "iocontrol.0.coolant-flood") {
            live.set_quick_flood(false);
        }
        if let Some(why) = checks::blocked(rows, checks::Feature::Probe) {
            live.set_probe_blocked(why.into());
        }
        if let Some(why) = checks::blocked(rows, checks::Feature::Job) {
            live.set_job_blocked(why.into());
        }
        if let Some(why) = checks::blocked(rows, checks::Feature::Conv) {
            live.set_conv_blocked(why.into());
        }
        let attention = rows.iter().filter(|c| c.level != checks::Level::Ok && c.level != checks::Level::Info).count();
        if attention > 0 {
            eprintln!("millgui: {attention} setup items need attention; run millgui --check {}", cfg.path.as_ref().map_or(String::new(), |p| p.display().to_string()));
        }
    }
    textentry::wire(&ui);
    // The sim browses its own folder (where CONV and JOB write in the sim),
    // seeded with the sample programs; PROGRAM_PREFIX is on the Pi.
    let files_root = if args.sim { sim_nc_dir() } else { files::root(&cfg) };
    files::wire(&ui, &handle, files_root);
    program::wire(&ui);
    let mut mdi_notes = Vec::new();
    let machine_cfg = machine_cfg::get();
    machine_cfg.apply(&ui);
    let plot = plot::wire(&ui, &cfg, &machine_cfg.model);
    let _plot_timer = plot::start_timer(&ui, &plot);
    if renderer == renderer::Choice::Gpu {
        if let Err(e) = plot::use_gpu(&ui) {
            eprintln!("millgui: GPU backplot unavailable ({e:?})");
        }
    }
    mdi::wire(&ui, &handle, &cfg, &mut mdi_notes);
    if !mdi_notes.is_empty() {
        live.set_message(mdi_notes.join("; ").into());
        live.set_message_error(true);
    }
    MANUAL.with(|m| *m.borrow_mut() = Some(manual));

    // Manual tool change: HAL through the drawbar module, or the simulator.
    let tc_io: Option<Rc<dyn toolchange::Io>> = if args.sim {
        let io = Rc::new(toolchange::SimIo::new(handle.clone()));
        SIM_TC.with(|t| *t.borrow_mut() = Some(io.clone()));
        Some(io)
    } else {
        modules.iter().find_map(|m| m.toolchange())
    };
    let tools = tools::wire(&ui, &handle, &cfg, tc_io.clone(), args.sim);
    let _tools_timer = tools::start_timer(&ui, &tools);
    TOOLS.with(|t| *t.borrow_mut() = Some(tools.clone()));
    let offsets = offsets::wire(&ui, &handle, &cfg, args.sim);
    // The simulator keeps its own timeline, beside its own tool table.
    let history_path = if args.sim { Some(job::cache_dir().join("sim-history.jsonl")) } else { settings::Settings::dir().map(|d| d.join("history.jsonl")) };
    history::wire(&ui, history_path, restore_fn(&offsets, &tools));
    let rot = offsets.clone();
    OFFSETS.with(|o| *o.borrow_mut() = Some(offsets));
    let probe = probe::wire(&ui, &handle, &cfg, tc_io.clone(), args.sim, Box::new(move |w| offsets::rotation(&rot, w)));
    let _probe_timer = probe::start_timer(&probe);
    PROBE.with(|p| *p.borrow_mut() = Some(probe));
    let setter = modules.iter().find_map(|m| m.setter());
    let util_state = if args.sim { settings::Settings::memory() } else { settings::Settings::at("util") };
    let (util, _util_timer) = util::wire(&ui, &handle, &cfg, tc_io.clone(), setter.clone(), util_state);
    UTIL.with(|u| *u.borrow_mut() = Some(util));
    // SETUP: the check's rows with their fixes. With no millgui.toml it
    // opens first (not in the simulator, nor when -screen names one).
    {
        let mut asked = machine_cfg::get().clone();
        asked.toolchange.style = None;
        let detected = checks::toolchange_style(&asked, &ids, hal.as_ref()).0;
        let first = !args.sim && args.screen.is_none() && machine_cfg::get().path.is_none();
        let s = setup::wire(&ui, &cfg, check_rows, ids.clone(), detected, setter, first);
        SETUP.with(|t| *t.borrow_mut() = Some(s));
    }
    let _tc_timer = tc_io.map(|io| toolchange::wire(&ui, &handle, &cfg, io));
    picker::wire(&ui);
    let ids: Vec<&str> = modules.iter().map(|m| m.id()).collect();
    let _job_timer = if ids.contains(&"job") || ids.contains(&"conv") {
        let (j, timer) = job::wire(&ui, &handle, &cfg, args.sim);
        let conv_state = if args.sim { settings::Settings::memory() } else { settings::Settings::at("conv") };
        let _ = conv::wire(&ui, j.clone(), conv_state);
        wire_job_probing(&ui, &j);
        JOB.with(|t| *t.borrow_mut() = Some(j));
        Some(timer)
    } else {
        None
    };

    // Remote (web) screen: [remote]port (or [MILLGUI]REMOTE_PORT) serves the browser build.
    let _remote = match machine_cfg.remote_port(&cfg) {
        Some(Ok(port)) => {
            let root = machine_cfg.remote_root(&cfg).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("millgui/web"));
            let root = if root.is_absolute() { root } else { cfg.dir().join(root) };
            match remote::start(&ui, port, root) {
                Ok((timer, r)) => {
                    remote::install(r);
                    Some(timer)
                }
                Err(e) => {
                    live.set_message(e.into());
                    live.set_message_error(true);
                    None
                }
            }
        }
        Some(Err(e)) => {
            live.set_message(e.into());
            live.set_message_error(true);
            None
        }
        None => None,
    };

    // Pills read HAL pins, which change without a status update; refresh them
    // with the clock and cycle timer.
    let tick = slint::Timer::default();
    {
        let axes = axes.clone();
        let weak = ui.as_weak();
        let state = state.clone();
        let modules = modules.clone();
        let last_message = RefCell::new(SharedString::default());
        let handle = handle.clone();
        tick.start(slint::TimerMode::Repeated, Duration::from_millis(200), move || {
            let Some(ui) = weak.upgrade() else { return };
            // Messages the screens raise go to the log too (LinuxCNC's are
            // logged as they arrive; the log skips a repeat of its top row).
            let msg = ui.global::<Live>().get_message();
            if *last_message.borrow() != msg {
                if !msg.is_empty() {
                    let error = ui.global::<Live>().get_message_error();
                    with_manual(|m| manual::log_text(m, &msg, error));
                }
                *last_message.borrow_mut() = msg;
            }
            let st = state.borrow();
            refresh_pills(&ui, &st, &modules, &axes);
            // On the clock, not on status: status is only sent when it
            // changes, and an idle machine sends none after a line is picked.
            if let Some(s) = st.status.as_ref() {
                restart_status(&ui, s, &handle);
            }
            let load = modules.iter().find_map(|m| m.load(st.status.as_ref()));
            plot::with(|p| plot::set_load(&ui, p, load));
            let live = ui.global::<Live>();
            let block: SharedString = modules.iter().find_map(|m| m.motion_block()).unwrap_or_default().into();
            if live.get_motion_block() != block {
                live.set_motion_block(block);
                with_manual(|m| manual::refresh(&ui, m));
            }
            let block: SharedString = modules.iter().find_map(|m| m.spindle_block()).unwrap_or_default().into();
            if live.get_spindle_block() != block {
                live.set_spindle_block(block);
            }
            live.set_clock(chrono::Local::now().format("%H:%M").to_string().into());
            let elapsed = st.cycle_started.map_or(st.cycle_elapsed, |t| t.elapsed());
            live.set_cycle_time(fmt_elapsed(elapsed).into());
        });
    }

    // The pendant's START/PAUSE, as the cycle buttons: start (with CYCLE
    // START's checks) or resume when idle or paused, feed hold when running.
    // The same tick passes its mode requests on (modules.rs, Pendant).
    let pendant_tick = slint::Timer::default();
    if modules.iter().any(|m| m.id() == "pendant") {
        let weak = ui.as_weak();
        let state = state.clone();
        let modules = modules.clone();
        pendant_tick.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
            let s = state.borrow().status.clone();
            // Every module ticks (no short-circuit): the pendant also passes
            // its mode requests on here.
            let pressed = modules.iter().fold(false, |any, m| m.tick(s.as_ref()) | any);
            if !pressed {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            let actions = ui.global::<Actions>();
            match s.map(|s| (s.interp, s.busy())) {
                Some((Interp::Paused, _)) | Some((Interp::Idle, false)) => {
                    toast(&ui, "Pendant START".to_string());
                    actions.invoke_cycle_start();
                }
                Some((_, true)) => {
                    toast(&ui, "Pendant PAUSE: feed hold".to_string());
                    actions.invoke_feed_hold();
                }
                _ => {}
            }
        });
    }

    // LinuxCNC stops the display with SIGTERM; leave the loop so hal_exit runs.
    let _ = ctrlc::set_handler(|| {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });

    ui.run()
}

/// `--write-ini`: the check's INI lines into the INI, only while LinuxCNC
/// is stopped (it reads the INI once, at start).
fn write_ini(cfg: &Config, rows: &[checks::Check]) {
    let Some(path) = &cfg.path else { return };
    if checks::Halcmd::new().running() {
        eprintln!("\nmillgui: LinuxCNC is running; stop it before --write-ini changes {}", path.display());
        std::process::exit(2);
    }
    match checks::write_ini(path, rows) {
        Ok(done) if done.is_empty() => println!("\nNothing to add to {}.", path.display()),
        Ok(done) => {
            println!("\nAdded to {} (the old file is beside it, .bak):", path.display());
            for l in done {
                println!("  {l}");
            }
        }
        Err(e) => {
            eprintln!("\nmillgui: {e}");
            std::process::exit(2);
        }
    }
}

fn set_nav(ui: &MainWindow, modules: &[Box<dyn Module>], screen: Option<&str>) {
    let live = ui.global::<Live>();
    let nav: Vec<NavItem> = modules
        .iter()
        .filter_map(|m| m.nav())
        .map(|n| NavItem { id: n.id.into(), label: n.label.into(), icon: n.icon.into(), badge: 0 })
        .collect();
    let first = nav.first().map(|n| n.id.clone()).unwrap_or_default();
    live.set_screen(screen.map(SharedString::from).unwrap_or(first));
    crate::models::sync(live.get_nav(), nav, |m| live.set_nav(m));
}

/// Scale for a desktop window of `w` x `h` pixels. The layout is designed
/// at 1920x1080. Height decides the scale, so text keeps its size: a wider
/// window gets wider screens, a narrower one narrower screens (they
/// rearrange down to `MIN_LAYOUT_WIDTH`), and only below that does the
/// whole layout shrink.
fn layout_scale(w: u32, h: u32) -> f32 {
    (w as f32 / MIN_LAYOUT_WIDTH).min(h as f32 / 1080.0)
}

/// The narrowest the screens are laid out, in layout pixels.
const MIN_LAYOUT_WIDTH: f32 = 1440.0;

/// Fit the 1920x1080 layout to a desktop window of any size.
///
/// The scale is set from winit's resize event before Slint handles it, so
/// Slint lays out the new size at the new scale in one step. (Changing the
/// scale afterwards leaves the window item at its old logical size, and the
/// winit backend then resizes the window to match it, which can feed back
/// into a runaway resize.) The OS display scale is ignored: the window's
/// size alone decides. At startup the window takes most of its screen.
fn fit_desktop_window(ui: &MainWindow) {
    use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
    let mut sized = false;
    ui.window().on_winit_window_event(move |w, ev| {
        // The first event: size the window to most of its screen. The
        // resize that follows sets the scale.
        if !sized {
            sized = true;
            w.with_winit_window(|ww| {
                let Some(m) = ww.current_monitor().or_else(|| ww.primary_monitor()) else {
                    eprintln!("millgui: no screen size known; the window keeps its size");
                    return;
                };
                let ms = m.size();
                // 16:9 in 85% of the screen.
                let k = (ms.width as f64 * 0.85 / 1920.0).min(ms.height as f64 * 0.85 / 1080.0);
                let _ = ww.request_inner_size(winit::dpi::PhysicalSize::new((1920.0 * k) as u32, (1080.0 * k) as u32));
            });
        }
        match ev {
            winit::event::WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                let target = layout_scale(size.width, size.height);
                if (target - w.scale_factor()).abs() > 0.0001 {
                    w.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: target });
                }
                EventResult::Propagate
            }
            // Moving to a screen with another pixel density: the resize
            // that follows sets the scale.
            winit::event::WindowEvent::ScaleFactorChanged { .. } => EventResult::PreventDefault,
            _ => EventResult::Propagate,
        }
    });
}

fn screenshot(args: &Args, cfg: &Config, modules: &[Box<dyn Module>], out: &Path) -> Result<(), slint::PlatformError> {
    use machine::Machine;
    let (w, h) = args.size.unwrap_or((1920, 1080));
    let window = headless::install(w, h);
    let ui = MainWindow::new()?;
    if args.size.is_some() {
        let s = layout_scale(w, h);
        ui.window().dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: s });
        ui.window().dispatch_event(slint::platform::WindowEvent::Resized { size: slint::LogicalSize::new(w as f32 / s, h as f32 / s) });
    }
    set_nav(&ui, modules, args.screen.as_deref());
    let mut sim = machine::Sim::new();
    for c in [
        Command::SetState(TaskState::EstopReset),
        Command::SetState(TaskState::On),
        Command::Home(JointSel::All),
        Command::ProgramOpen(args.open.clone().unwrap_or_else(|| sample_dir().join("bracket-sample.ngc"))),
    ] {
        let _ = sim.send(&c);
    }
    let s = machine::Status::from(sim.poll().map_err(|e| slint::PlatformError::Other(e.to_string()))?.clone());
    let mut st = UiState { connected: true, ..Default::default() };
    let live = ui.global::<Live>();
    live.set_connected(true);
    apply_status(&ui, &mut st, &s, &axis_indices());
    refresh_pills(&ui, &st, modules, &axis_indices());
    live.set_clock(chrono::Local::now().format("%H:%M").to_string().into());
    live.set_message("Simulated machine".into());

    let handle = machine::spawn(Box::new(machine::Sim::new()), Duration::from_secs(3600), |_| {});
    let manual = manual::wire(&ui, &handle, cfg, settings::Settings::memory(), axis_indices().contains(&3));
    textentry::wire(&ui);
    mdi::wire(&ui, &handle, cfg, &mut Vec::new());
    files::wire(&ui, &handle, sample_dir());
    program::wire(&ui);
    program::sync(&ui, &s.file, false);
    let machine_cfg = machine_cfg::get();
    machine_cfg.apply(&ui);
    let plot = plot::wire(&ui, cfg, &machine_cfg.model);
    plot::load_now(&ui, &plot, &s.file);
    plot::tool_moved(&plot, tool_tip(&s));
    plot::machine_pose(&plot, machine_pose(&ui, &s));
    manual::apply_status(&ui, &manual, &s);
    let tc_io = Rc::new(toolchange::SimIo::new(handle.clone()));
    if args.show.iter().any(|w| w == "tc") {
        let mut pending = s.clone();
        pending.sim_tool_change = Some(214);
        tc_io.status(&pending);
    }
    let tools = tools::wire(&ui, &handle, cfg, Some(tc_io.clone() as Rc<dyn toolchange::Io>), true);
    tools::apply_status(&ui, &tools, &s);
    let offsets = offsets::wire(&ui, &handle, cfg, true);
    offsets::apply_status(&ui, &offsets, &s);
    plot::set_live(&ui, &plot, offsets::preflight_live(&offsets, &s));
    history::wire(&ui, None, restore_fn(&offsets, &tools));
    let rot = offsets.clone();
    let probe = probe::wire(&ui, &handle, cfg, Some(tc_io.clone() as Rc<dyn toolchange::Io>), true, Box::new(move |w| offsets::rotation(&rot, w)));
    probe::apply_status(&ui, &probe, &s);
    let (util, _util_timer) = util::wire(&ui, &handle, cfg, Some(tc_io.clone() as Rc<dyn toolchange::Io>), None, settings::Settings::memory());
    util::apply_status(&util, &s);
    let _tc_timer = toolchange::wire(&ui, &handle, cfg, tc_io);
    picker::wire(&ui);
    let (j, _job_timer) = job::wire(&ui, &handle, cfg, true);
    job::apply_status(&j, &s);
    let conv = conv::wire(&ui, j.clone(), settings::Settings::memory());
    let actions = ui.global::<Actions>();
    for what in &args.show {
        match what.as_str() {
            "keypad" => {
                actions.invoke_edit("jog-speed".into());
                actions.invoke_keypad_key("CLR".into());
                for k in ["2", "5"] {
                    actions.invoke_keypad_key(k.into());
                }
            }
            // "keys:<value>:<typed>" opens the keypad on a value and types
            // into it as a keyboard would; a trailing '=' presses ENTER.
            w if w.starts_with("keys:") => {
                let mut p = w.splitn(3, ':').skip(1);
                let (Some(target), Some(typed)) = (p.next(), p.next()) else { continue };
                actions.invoke_edit(target.into());
                for k in typed.trim_end_matches('=').chars() {
                    actions.invoke_keypad_key(k.to_string().into());
                }
                if typed.ends_with('=') {
                    actions.invoke_keypad_done(true);
                }
            }
            "mdi" => {
                live.set_run_tab("mdi".into());
                for l in ["G0 X0 Y0", "M6 T12", "S8000 M3"] {
                    actions.invoke_mdi_send(l.into());
                }
                actions.invoke_mdi_category_set("OFFSET".into());
                actions.invoke_mdi_pick("G10 L20 P1 X".into());
                for k in ["5", "Y", "0"] {
                    actions.invoke_mdi_key(k.into());
                }
            }
            // The text keyboard on the first tool's comment.
            "comment" => {
                actions.invoke_tools_select(0);
                actions.invoke_text_edit("tool-comment".into());
                for k in ["SPACE", "6", "M", "M"] {
                    actions.invoke_text_key(k.into());
                }
            }
            "line" => {
                live.set_line(18);
                live.set_running(true);
            }
            "file" => actions.invoke_files_tap(1),
            // "file:<row>" selects that row; "file-op:<n>" picks out an op.
            w if w.starts_with("file:") => actions.invoke_files_tap(w[5..].parse().unwrap_or(1)),
            w if w.starts_with("file-op:") => actions.invoke_files_op_tap(w[8..].parse().unwrap_or(0)),
            // PROBE state: probe-mode:<m>, probe-shape:<s>, probe-target:<t>.
            w if w.starts_with("probe-") => {
                if let Some((k, v)) = w.split_once(':') {
                    match k {
                        "probe-mode" => actions.invoke_probe_mode(v.into()),
                        "probe-shape" => actions.invoke_probe_shape(v.into()),
                        "probe-target" => actions.invoke_probe_target(v.into()),
                        "probe-opt" => actions.invoke_probe_opt(v.into()),
                        // The preview held at <ms> into its loop.
                        "probe-at" => probe_anim::freeze(&ui, v.parse().unwrap_or(0.0)),
                        _ => {}
                    }
                }
            }
            "file-code" => live.set_files_tab("code".into()),
            "jog" => live.set_run_tab("jog".into()),
            "feed" | "goto" => live.set_jog_tab(what.as_str().into()),
            "gotos" => {
                for (x, y) in [(0.0, 0.0), (-12.0, 0.0), (50.0, -30.0), (150.0, -150.0)] {
                    actions.invoke_edit("gt-x".into());
                    actions.invoke_keypad_key("CLR".into());
                    for k in format!("{x}").chars() {
                        actions.invoke_keypad_key(if k == '-' { "±".into() } else { k.to_string().into() });
                    }
                    actions.invoke_keypad_done(true);
                    actions.invoke_edit("gt-y".into());
                    actions.invoke_keypad_key("CLR".into());
                    for k in format!("{y}").chars() {
                        actions.invoke_keypad_key(if k == '-' { "±".into() } else { k.to_string().into() });
                    }
                    actions.invoke_keypad_done(true);
                    actions.invoke_goto_save();
                }
                actions.invoke_goto_select(2);
            }
            "messages" => {
                live.set_drawer_open(true);
                for (kind, text) in [
                    (MessageKind::Text, "Probe tripped at Z-12.402"),
                    (MessageKind::Display, "(MSG, Check that the vise is clamped)"),
                    (MessageKind::Error, "Can't do MDI when machine is not on"),
                ] {
                    manual::log_message(&ui, &manual, &machine::Message { kind, text: text.into() });
                }
            }
            "timeline" => {
                live.set_offsets_view("timeline".into());
                use history::Target::{Tool, Wcs};
                history::record(Wcs(0), &[0.0; 5], &[215.638, -94.431, -322.444, 0.0, 0.0], "During MDI or a probe routine", [0.0; 2]);
                history::record(Tool(7), &[0.0, 4.0], &[109.232, 4.0], "Tool table changed outside millgui", [0.0; 2]);
                history::record(Wcs(0), &[215.638, -94.431, -322.444, 0.0, 0.0], &[215.638, -94.431, -322.394, 0.0, 0.0], "Zero Z here", [0.0; 2]);
                history::record(Tool(7), &[109.232, 4.0], &[109.212, 4.0], "Wear adjust -0.020 length", [-0.02, 0.0]);
                history::record(Wcs(1), &[0.0; 5], &[120.0, -40.0, -300.0, 0.0, 1.5], "During program bracket-sample.ngc", [0.0; 2]);
                history::record(Wcs(0), &[215.638, -94.431, -322.394, 0.0, 0.0], &[0.0; 5], "Cleared on OFFSETS", [0.0; 2]);
                // Show a wear adjustment beside T7's length and T9's diameter.
                history::set_wear(7, 0, -0.02, 109.232434);
                history::set_wear(9, 1, 0.015, 5.0);
                actions.invoke_tools_reload();
                actions.invoke_history_select(3);
            }
            "load" => {
                // A cut along the sample part with the load rising and falling.
                for i in 0..=120 {
                    let t = i as f64 / 120.0;
                    plot::set_load(&ui, &plot, Some((t * std::f64::consts::PI * 3.0).sin().abs() * (0.35 + 0.65 * t)));
                    plot::tool_moved(&plot, [-30.0 + 160.0 * t, 15.0 + 25.0 * (t * 9.0).sin(), 0.0, 0.0]);
                }
            }
            "pick" => actions.invoke_job_add_step(),
            t if t.starts_with("conv-sec-") => actions.invoke_conv_section(t[9..].into()),
            t if t.starts_with("conv-") => actions.invoke_conv_type(t[5..].into()),
            other => eprintln!("millgui: unknown --show {other}"),
        }
    }
    let _ = &conv;
    ui.show()?;
    headless::save(&window, out).map_err(slint::PlatformError::Other)?;
    eprintln!("millgui: wrote {}", out.display());
    Ok(())
}

/// The timeline's restore: the screen that owns the values makes the edit.
fn restore_fn(offsets: &Rc<RefCell<offsets::Offsets>>, tools: &Rc<RefCell<tools::Tools>>) -> history::Restore {
    let (o, t) = (offsets.clone(), tools.clone());
    Box::new(move |e, wear, cause| match e.target {
        history::Target::Wcs(sys) => offsets::restore(&o, sys, &e.before(), cause),
        history::Target::Tool(n) => tools::restore(&t, n, &e.before(), wear, cause),
    })
}

/// Sample programs for headless screenshots (millgui/testdata/nc_files).
fn sim_nc_dir() -> PathBuf {
    let dir = job::cache_dir().join("sim-nc");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(rd) = std::fs::read_dir(sample_dir()) {
        for e in rd.flatten() {
            let to = dir.join(e.file_name());
            if e.path().is_file() && !to.exists() {
                let _ = std::fs::copy(e.path(), to);
            }
        }
    }
    dir
}

fn sample_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../testdata/nc_files")
}

type UiEventFn = Box<dyn FnMut(&MainWindow, Event)>;

thread_local! {
    static SIM_TC: RefCell<Option<Rc<toolchange::SimIo>>> = const { RefCell::new(None) };
    static TOOLS: RefCell<Option<Rc<RefCell<tools::Tools>>>> = const { RefCell::new(None) };
    static OFFSETS: RefCell<Option<Rc<RefCell<offsets::Offsets>>>> = const { RefCell::new(None) };
    static PROBE: RefCell<Option<Rc<RefCell<probe::Probe>>>> = const { RefCell::new(None) };
    static UTIL: RefCell<Option<Rc<RefCell<util::Util>>>> = const { RefCell::new(None) };
    static JOB: RefCell<Option<Rc<RefCell<job::Job>>>> = const { RefCell::new(None) };
    static SETUP: RefCell<Option<Rc<RefCell<setup::Setup>>>> = const { RefCell::new(None) };
    /// The status handler lives on the UI thread; the machine thread only
    /// posts events to it.
    static UI_EVENT: RefCell<Option<UiEventFn>> = RefCell::new(None);
    static MANUAL: RefCell<Option<Rc<RefCell<manual::Manual>>>> = RefCell::new(None);
    /// RUN FROM SELECTED in progress (its MDI prelude), if any.
    static RESTART: RefCell<Option<restart::Restart>> = const { RefCell::new(None) };
    /// (restart copy, original): status naming the copy is shown as the
    /// original, so the program view, plot and line numbers stay put.
    static RESTART_COPY: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    /// (selected line, loaded tool, program) the restart summary was made for.
    static RESTART_SHOWN: RefCell<(i32, i32, String)> = RefCell::new((-2, -1, String::new()));
}

/// Folders the interpreter finds subroutines and remaps in
/// ([RS274NGC] SUBROUTINE_PATH and USER_M_PATH), resolved.
fn subroutine_dirs(cfg: &Config) -> Vec<PathBuf> {
    ["SUBROUTINE_PATH", "USER_M_PATH"]
        .iter()
        .filter_map(|k| cfg.get("RS274NGC", k))
        .flat_map(|v| v.split(':').map(str::trim).filter(|d| !d.is_empty()).map(|d| cfg.dir().join(d)).collect::<Vec<_>>())
        .filter_map(|d| d.canonicalize().ok())
        .collect()
}

thread_local! {
    /// (file LinuxCNC reported, file to show) for the last status.
    static PROGRAM_FILE: RefCell<(String, String)> = RefCell::new(Default::default());
}

/// LinuxCNC's status names the file the interpreter is reading, which is
/// the M6 remap or another subroutine whenever it reads ahead into one (or
/// an MDI command calls one). Show the loaded program instead: a file in a
/// subroutine folder keeps the program shown before it, and the restart
/// copy shows as the program it was made from.
fn program_file(s: &mut Status, sub_dirs: &[PathBuf]) {
    PROGRAM_FILE.with(|pf| {
        let mut pf = pf.borrow_mut();
        if s.file == pf.0 {
            s.file = pf.1.clone();
            return;
        }
        let raw = std::mem::take(&mut s.file);
        let in_subs = Path::new(&raw)
            .parent()
            .and_then(|d| d.canonicalize().ok())
            .is_some_and(|d| sub_dirs.contains(&d));
        let shown = if in_subs {
            pf.1.clone()
        } else {
            RESTART_COPY.with(|c| match c.borrow().as_ref() {
                Some((copy, original)) if raw == *copy => original.clone(),
                _ => raw.clone(),
            })
        };
        s.file = shown.clone();
        *pf = (raw, shown);
    });
}

fn with_probe(f: impl FnOnce(&mut probe::Probe)) {
    PROBE.with(|p| {
        if let Some(p) = p.borrow().as_ref() {
            f(&mut p.borrow_mut());
        }
    });
}

/// PROBE screen -> JOB page (+ JOB / + CHECK), and a check's tool
/// correction -> the tool table.
fn wire_job_probing(ui: &MainWindow, j: &Rc<RefCell<job::Job>>) {
    let actions = ui.global::<Actions>();
    let (t, w) = (j.clone(), ui.as_weak());
    actions.on_probe_add_job(move |check| {
        let Some(ui) = w.upgrade() else { return };
        let made = PROBE.with(|p| p.borrow().as_ref().map(|p| p.borrow().job_step(check)));
        let r = match made {
            Some(Ok((name, params))) => t.borrow_mut().add_probe(&name, params),
            Some(Err(e)) => Err(e),
            None => return,
        };
        let live = ui.global::<Live>();
        match r {
            Ok(m) => {
                // "ADDED BORE CENTER AS OP 2." -> one sentence with where it went.
                // A check starts at a work position, so it only means
                // something when taught with the work offset in place.
                let start = if check {
                    "It starts at this spot on the part, so add it with the work offset already set."
                } else {
                    "It starts where the probe is now."
                };
                let text = format!("{} on the JOB page. {start}", m.trim_end_matches('.'));
                live.set_message(text.clone().into());
                live.set_message_error(false);
                // Too long for the top bar, and it says where the op went.
                toast(&ui, text);
                live.set_toast_info(true);
            }
            Err(e) => {
                live.set_message(e.clone().into());
                live.set_message_error(true);
                toast(&ui, e);
            }
        }
    });
    let (t, w) = (j.clone(), ui.as_weak());
    actions.on_job_check_adjust(move || {
        let Some(ui) = w.upgrade() else { return };
        let id = t.borrow().selected_id();
        let Some((tool, d, _)) = id.as_deref().and_then(|id| t.borrow().adjustment(id)) else { return };
        let r = TOOLS.with(|ts| match ts.borrow().as_ref() {
            Some(ts) => tools::adjust_diameter(ts, tool, d, "Job check correction"),
            None => Err("The tool table is not open".into()),
        });
        let live = ui.global::<Live>();
        match r {
            Ok(()) => {
                live.set_message(format!("T{tool} diameter is now {d:.3}. BUILD AND LOAD to cut with it.").into());
                live.set_message_error(false);
                if let Some(id) = id {
                    t.borrow_mut().mark_corrected(&id);
                }
            }
            Err(e) => {
                live.set_message(e.clone().into());
                live.set_message_error(true);
                toast(&ui, e);
            }
        }
    });
}

fn gcode_lines(ui: &MainWindow) -> Vec<SharedString> {
    use slint::Model;
    ui.global::<Live>().get_gcode_lines().iter().collect()
}

/// Keep the line under the program list saying what RUN FROM SELECTED will
/// expect, and advance a restart that is under way.
fn restart_status(ui: &MainWindow, s: &Status, m: &Handle) {
    let live = ui.global::<Live>();
    let sel = live.get_gcode_selected();
    let key = (sel, s.tool_in_spindle, s.file.clone());
    let stale = RESTART_SHOWN.with(|k| *k.borrow() != key);
    if stale {
        let text = if sel < 0 {
            String::new()
        } else {
            restart::plan(&gcode_lines(ui), sel as usize).summary(s.tool_in_spindle)
        };
        live.set_gcode_restart(text.into());
        RESTART_SHOWN.with(|k| *k.borrow_mut() = key);
    }
    RESTART.with(|r| {
        let mut r = r.borrow_mut();
        if let Some(run) = r.as_mut() {
            if !run.pump(s, m) {
                *r = None;
            }
        }
    });
}


thread_local! {
    static TOAST: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static WAS_PAUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LAST_MSG: RefCell<Option<(String, Instant)>> = const { RefCell::new(None) };
}

/// Show a refusal over the bottom bar for a few seconds.
/// A toast that stays until tapped (or replaced): for errors that must be
/// read in full.
fn toast_until_tapped(ui: &MainWindow, text: String) {
    // Moving the counter on cancels the timer of any earlier toast.
    TOAST.with(|t| t.set(t.get() + 1));
    ui.global::<Live>().set_toast_info(false);
    ui.global::<Live>().set_toast(text.into());
}

/// LinuxCNC's error, plus what to do for the ones whose cause is not on
/// the line they name.
fn explain_error(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let hint = if lower.contains("cannot use g53 incremental") {
        Some("G91 (incremental) is still on from MDI or a macro. Put G90 before the G53, or run G90 in MDI.")
    } else if lower.contains("cannot use g53 with cutter radius comp") {
        Some("Cutter compensation is still on. Put G40 before the G53, or run G40 in MDI.")
    } else if lower.contains("queue is not empty after probing") {
        // A timing fault inside LinuxCNC (docs/reviews/2026-11-17-queue-not-empty-after-probing.md).
        Some("LinuxCNC lost track of the probe move. Nothing was written. Start the job or the probe again.")
    } else {
        None
    };
    match hint {
        Some(h) => format!("{text}\n{h}"),
        None => text.to_string(),
    }
}

fn toast(ui: &MainWindow, text: String) {
    ui.global::<Live>().set_toast_info(false);
    ui.global::<Live>().set_toast(text.into());
    let n = TOAST.with(|t| {
        t.set(t.get() + 1);
        t.get()
    });
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_secs(5), move || {
        if TOAST.with(|t| t.get()) == n {
            if let Some(ui) = weak.upgrade() {
                ui.global::<Live>().set_toast("".into());
            }
        }
    });
}

fn with_manual(f: impl FnOnce(&Rc<RefCell<manual::Manual>>)) {
    if let Some(m) = MANUAL.with(|m| m.borrow().clone()) {
        f(&m);
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn the_status_pill_names_the_worst_row() {
        let ok = [Pill::new("HOMED XYZ", "good"), Pill::new("PROBE NOT LOADED", ""), Pill::new("SPINDLE CMD CW 4000", "info")];
        assert_eq!(status_summary(&ok), ("ALL OK".to_string(), "good", false));
        let warn = [Pill::new("HOMED XYZ", "good"), Pill::new("PROBE LOADED", "warn")];
        assert_eq!(status_summary(&warn), ("PROBE LOADED".to_string(), "warn", false));
        // A fault comes before a warning listed ahead of it, and flashes.
        let bad = [Pill::new("PROBE LOADED", "warn"), Pill::new("DRIVE FAULT X", "bad"), Pill::new("NOT HOMED", "bad")];
        assert_eq!(status_summary(&bad), ("DRIVE FAULT X +2".to_string(), "bad", true));
    }
}
