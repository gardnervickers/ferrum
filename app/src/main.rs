//! millgui: the mill's LinuxCNC display program.
//!
//! LinuxCNC starts it as `millgui -ini <file>`. `--sim` runs against the
//! simulated machine instead (the default on builds without NML, e.g. the Mac).

mod config;
mod conv;
mod files;
mod headless;
mod job;
mod machine_cfg;
mod manual;
mod mdi;
mod modules;
mod picker;
mod plot;
mod powerfeed;
mod program;
mod remote;
mod settings;
mod toolchange;
mod offsets;
mod probe;
mod tools;
mod util;

use config::Config;
use linuxcnc::{Command, Event, Handle, Interp, MessageKind, Status, TaskState, AXIS_LETTERS};
use modules::{Module, Pill};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
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
    /// Screenshot extras: "keypad", "messages", "mdi".
    show: Vec<String>,
}

fn parse_args() -> Args {
    let mut a = Args { ini: std::env::var_os("INI_FILE_NAME").map(PathBuf::from), sim: false, windowed: false, screenshot: None, screen: None, show: Vec::new() };
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
            _ => {}
        }
    }
    if !cfg!(target_os = "linux") {
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

fn connect_machine(args: &Args, cfg: &Config) -> Box<dyn linuxcnc::Machine> {
    if args.sim {
        return Box::new(linuxcnc::Sim::new());
    }
    let nml = cfg.get("EMC", "NML_FILE").map(|f| cfg.dir().join(f).to_string_lossy().into_owned());
    // LinuxCNC starts the display after task is up, but give it a moment.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match linuxcnc::connect(nml.as_deref()) {
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
fn setup_hal(cfg: &Config, modules: &mut [Box<dyn Module>], notes: &mut Vec<String>) -> Option<hal::Component> {
    let comp = match hal::Component::new("millgui") {
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
        notes.push(e);
        return Some(comp);
    }
    let Some(ini) = cfg.path.as_ref() else { return Some(comp) };
    let mut files: Vec<PathBuf> = modules
        .iter()
        .map(|m| PathBuf::from(format!("millgui/hal/{}.hal", m.id())))
        .filter(|p| cfg.dir().join(p).is_file())
        .collect();
    if let Some(extra) = cfg.get("MILLGUI", "POSTGUI_HALFILE") {
        files.push(PathBuf::from(extra));
    }
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

fn fmt_axis(v: f64, letter: char) -> String {
    if matches!(letter, 'A' | 'B' | 'C') {
        format!("{v:.2}°")
    } else {
        format!("{v:.3}")
    }
}

fn axis_indices(cfg: &Config) -> Vec<usize> {
    let mut out = Vec::new();
    for c in cfg.coordinates().chars() {
        if let Some(i) = AXIS_LETTERS.iter().position(|a| *a == c.to_ascii_uppercase()) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    if out.is_empty() {
        out = vec![0, 1, 2];
    }
    out
}

/// Mode and homing pills. A has no home switch: homing "references" it
/// where it stands, so the pill says A REF rather than implying A is homed.
fn core_pills_front(s: &Status, axes: &[usize], out: &mut Vec<Pill>) {
    out.push(if s.estop() {
        Pill::new("E-STOP", "bad")
    } else if !s.power_on() {
        Pill::new("OFF", "warn")
    } else {
        match s.mode {
            linuxcnc::Mode::Manual => Pill::new("MANUAL", "info"),
            linuxcnc::Mode::Auto if s.interp == Interp::Paused => Pill::new("PAUSED", "warn"),
            linuxcnc::Mode::Auto => Pill::new("AUTO", "good"),
            linuxcnc::Mode::Mdi => Pill::new("MDI", "info"),
        }
    });
    let homed = |i: usize| s.homed.get(i).copied().unwrap_or(false);
    let linear: Vec<usize> = axes.iter().copied().filter(|&i| i < 3).collect();
    let rotary: Vec<usize> = axes.iter().copied().filter(|&i| (3..6).contains(&i)).collect();
    if !linear.iter().all(|&i| homed(i)) {
        out.push(Pill::new("NOT HOMED", "bad"));
        return;
    }
    let mut text = format!("HOMED {}", linear.iter().map(|&i| AXIS_LETTERS[i]).collect::<String>());
    let mut kind = "good";
    for &i in &rotary {
        if homed(i) {
            text.push_str(&format!(" · {} REF", AXIS_LETTERS[i]));
        } else {
            text.push_str(&format!(" · {} NO REF", AXIS_LETTERS[i]));
            kind = "warn";
        }
    }
    out.push(Pill::new(text, kind));
}

/// Commanded spindle state (there is no speed feedback to show).
fn core_pills_back(s: &Status, out: &mut Vec<Pill>) {
    out.push(if s.spindle_direction == 0 || s.spindle_speed == 0.0 {
        Pill::new("SPINDLE CMD OFF", "")
    } else {
        Pill::new(
            format!(
                "SPINDLE CMD {} {:.0}",
                if s.spindle_direction > 0 { "CW" } else { "CCW" },
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
    let model: Vec<PillData> =
        pills.into_iter().map(|p| PillData { text: p.text.into(), kind: p.kind.into() }).collect();
    ui.global::<Live>().set_pills(ModelRc::new(VecModel::from(model)));
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
    let rows: Vec<DroRow> = axes
        .iter()
        .map(|&i| {
            let l = AXIS_LETTERS[i];
            // Joint i maps to axis i on this trivkins machine.
            DroRow {
                axis: l.to_string().into(),
                work: fmt_axis(work[i], l).into(),
                mach: fmt_axis(s.actual[i], l).trim_end_matches('°').into(),
                dtg: fmt_axis(s.dtg[i], l).trim_end_matches('°').into(),
                homed: s.homed.get(i).copied().unwrap_or(false),
            }
        })
        .collect();
    live.set_dro(ModelRc::new(VecModel::from(rows)));
    live.set_estop(s.estop());
    live.set_power(s.power_on());
    live.set_homed(axes.iter().all(|&i| s.homed.get(i).copied().unwrap_or(false)));
    live.set_wcs(s.wcs_name().into());
    live.set_tool(format!("T{}", s.tool_in_spindle).into());
    live.set_tc_from(format!("T{}", s.tool_in_spindle).into());
    live.set_feed(format!("{:.0}", s.feed_per_min()).into());
    let rpm = s.spindle_speed * s.spindle_scale * s.spindle_direction.signum() as f64;
    live.set_rpm(format!("{:.0}", rpm.abs()).into());
    live.set_modal(s.modal_string().into());
    live.set_feed_pct((s.feed_scale * 100.0).round() as i32);
    live.set_rapid_pct((s.rapid_scale * 100.0).round() as i32);
    live.set_spindle_pct((s.spindle_scale * 100.0).round() as i32);
    live.set_program(
        Path::new(&s.file).file_name().map(|f| f.to_string_lossy().to_uppercase()).unwrap_or_default().into(),
    );
    // LinuxCNC keeps the last run's lines after it ends; idle shows none.
    live.set_line(if s.interp == Interp::Idle { 0 } else { s.current_line.max(s.motion_line) });
    live.set_running(s.busy());
    live.set_paused(s.interp == Interp::Paused);
    live.set_optional_stop(s.optional_stop);
    let ready = s.power_on() && s.all_homed();
    live.set_can_start(ready && (s.interp == Interp::Paused || (s.interp == Interp::Idle && !s.file.is_empty())));

    // Cycle timer: runs while a program is running in auto mode.
    let running = s.mode == linuxcnc::Mode::Auto && s.busy();
    let was = st.status.as_ref().is_some_and(|p| p.mode == linuxcnc::Mode::Auto && p.busy());
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
    actions.on_home_all(move || m.send(Command::Home(-1)));
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
    let probe_tool = cfg.get("PROBE", "TOOL_NUMBER").and_then(|v| v.trim().parse::<i32>().ok()).unwrap_or(99);
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
            let problem = if s.tool_in_spindle == probe_tool {
                Some("Unload the probe before cycle start".to_string())
            } else if !block.is_empty() {
                Some(block.to_string())
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
        }
        match (s.interp, single) {
            (Interp::Paused, true) => m.send(Command::Step),
            (Interp::Paused, false) => m.send(Command::Resume),
            (Interp::Idle, true) => {
                m.send(Command::SetMode(linuxcnc::Mode::Auto));
                m.send(Command::Step);
            }
            (Interp::Idle, false) => m.send(Command::ProgramRun(0)),
            _ => {}
        }
    });
    let m = machine.clone();
    actions.on_feed_hold(move || m.send(Command::Pause));
    let m = machine.clone();
    actions.on_stop(move || m.send(Command::Abort));
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
    let (mut modules, problems) = modules::build(&cfg);
    notes.extend(problems);

    if let Some(out) = &args.screenshot {
        return screenshot(&args, &cfg, &modules, out);
    }

    let machine = connect_machine(&args, &cfg);
    // Keep the HAL component alive until exit.
    let _hal = if args.sim { None } else { setup_hal(&cfg, &mut modules, &mut notes) };

    let ui = MainWindow::new()?;
    if !args.windowed && !args.sim {
        ui.window().set_fullscreen(true);
    }
    // A desktop window may be any size: scale the 1920x1080 layout to fit.
    let fit = slint::Timer::default();
    if args.windowed || args.sim {
        let weak = ui.as_weak();
        fit.start(slint::TimerMode::Repeated, Duration::from_millis(200), move || {
            let Some(ui) = weak.upgrade() else { return };
            let w = ui.window();
            let s = w.size();
            if s.width == 0 || s.height == 0 {
                return;
            }
            let target = (s.width as f32 / 1920.0).min(s.height as f32 / 1080.0);
            if (target - w.scale_factor()).abs() > 0.01 {
                w.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: target });
            }
        });
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

    let axes = axis_indices(&cfg);
    let state = Rc::new(RefCell::new(UiState::default()));
    let modules = Rc::new(modules);

    let weak = ui.as_weak();
    let handle = linuxcnc::spawn(machine, Duration::from_millis(50), move |ev| {
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
        UI_EVENT.with(|f| {
            *f.borrow_mut() = Some(Box::new(move |ui: &MainWindow, ev: Event| {
                let live = ui.global::<Live>();
                match ev {
                    Event::Status(s) => {
                        let mut st = state.borrow_mut();
                        if st.lost {
                            st.lost = false;
                            live.set_message("Reconnected to LinuxCNC".into());
                            live.set_message_error(false);
                        }
                        st.connected = true;
                        live.set_connected(true);
                        apply_status(ui, &mut st, &s, &axes);
                        with_manual(|mm| manual::apply_status(ui, mm, &s));
                        program::sync(ui, &s.file);
                        SIM_TC.with(|t| {
                            if let Some(t) = t.borrow().as_ref() {
                                t.status(&s);
                            }
                        });
                        plot::sync(ui, &s.file);
                        // After the tools, which know the loaded tool's size.
                        TOOLS.with(|t| {
                            if let Some(t) = t.borrow().as_ref() {
                                tools::apply_status(ui, t, &s);
                            }
                        });
                        plot::with(|p| {
                            plot::tool_moved(p, tool_tip(&s));
                            plot::machine_pose(p, machine_pose(ui, &s));
                        });
                        OFFSETS.with(|o| {
                            if let Some(o) = o.borrow().as_ref() {
                                offsets::apply_status(ui, o, &s);
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
                    }
                    Event::Message(m) => {
                        // Macros report results as DEBUG lines.
                        if UTIL.with(|u| u.borrow().as_ref().is_some_and(|u| util::message(u, &m))) {
                            return;
                        }
                        if PROBE.with(|p| p.borrow().as_ref().is_some_and(|p| probe::message(ui, p, &m))) {
                            return;
                        }
                        with_manual(|mm| manual::log_message(ui, mm, &m));
                        live.set_message(SharedString::from(m.text.trim()));
                        live.set_message_error(m.kind == MessageKind::Error);
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
    // The sim browses its own folder (where CONV and JOB write in the sim),
    // seeded with the sample programs; PROGRAM_PREFIX is on the Pi.
    let files_root = if args.sim { sim_nc_dir() } else { files::root(&cfg) };
    files::wire(&ui, &handle, files_root);
    program::wire(&ui);
    let mut mdi_notes = Vec::new();
    let machine_cfg = machine_cfg::MachineCfg::load(&cfg, &mut mdi_notes);
    machine_cfg.apply(&ui);
    let plot = plot::wire(&ui, &cfg, &machine_cfg.model);
    let _plot_timer = plot::start_timer(&ui, &plot);
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
    TOOLS.with(|t| *t.borrow_mut() = Some(tools));
    let offsets = offsets::wire(&ui, &handle, &cfg, args.sim);
    let rot = offsets.clone();
    OFFSETS.with(|o| *o.borrow_mut() = Some(offsets));
    let probe = probe::wire(&ui, &handle, &cfg, tc_io.clone(), args.sim, Box::new(move |w| offsets::rotation(&rot, w)));
    let _probe_timer = probe::start_timer(&probe);
    PROBE.with(|p| *p.borrow_mut() = Some(probe));
    let setter = modules.iter().find_map(|m| m.setter());
    let util_state = if args.sim { settings::Settings::memory() } else { settings::Settings::at("util") };
    let (util, _util_timer) = util::wire(&ui, &handle, &cfg, tc_io.clone(), setter, util_state);
    UTIL.with(|u| *u.borrow_mut() = Some(util));
    let _tc_timer = tc_io.map(|io| toolchange::wire(&ui, &handle, &cfg, io));
    picker::wire(&ui);
    let ids: Vec<&str> = modules.iter().map(|m| m.id()).collect();
    let _job_timer = if ids.contains(&"job") || ids.contains(&"conv") {
        let (j, timer) = job::wire(&ui, &handle, &cfg, args.sim);
        let conv_state = if args.sim { settings::Settings::memory() } else { settings::Settings::at("conv") };
        let _ = conv::wire(&ui, j.clone(), conv_state);
        JOB.with(|t| *t.borrow_mut() = Some(j));
        Some(timer)
    } else {
        None
    };

    // Remote (web) screen: [MILLGUI]REMOTE_PORT serves the browser build.
    let _remote = match cfg.get("MILLGUI", "REMOTE_PORT").map(|p| p.trim().parse::<u16>()) {
        Some(Ok(port)) => {
            let root = cfg.get("MILLGUI", "REMOTE_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("millgui/web"));
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
        Some(Err(_)) => {
            live.set_message("[MILLGUI]REMOTE_PORT is not a port number".into());
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

    // LinuxCNC stops the display with SIGTERM; leave the loop so hal_exit runs.
    let _ = ctrlc::set_handler(|| {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });

    ui.run()
}

fn set_nav(ui: &MainWindow, modules: &[Box<dyn Module>], screen: Option<&str>) {
    let live = ui.global::<Live>();
    let nav: Vec<NavItem> = modules
        .iter()
        .filter_map(|m| m.nav())
        .map(|n| NavItem { id: n.id.into(), label: n.label.into(), icon: n.icon.into() })
        .collect();
    let first = nav.first().map(|n| n.id.clone()).unwrap_or_default();
    live.set_screen(screen.map(SharedString::from).unwrap_or(first));
    live.set_nav(ModelRc::new(VecModel::from(nav)));
}

/// Render one frame of a simulated, powered-on, homed machine to a PNG.
fn screenshot(args: &Args, cfg: &Config, modules: &[Box<dyn Module>], out: &Path) -> Result<(), slint::PlatformError> {
    use linuxcnc::Machine;
    let window = headless::install(1920, 1080);
    let ui = MainWindow::new()?;
    set_nav(&ui, modules, args.screen.as_deref());
    let mut sim = linuxcnc::Sim::new();
    for c in [
        Command::SetState(TaskState::EstopReset),
        Command::SetState(TaskState::On),
        Command::Home(-1),
        Command::ProgramOpen(sample_dir().join("bracket-sample.ngc").to_string_lossy().into_owned()),
    ] {
        let _ = sim.send(&c);
    }
    let s = sim.poll().map_err(|e| slint::PlatformError::Other(e))?;
    let mut st = UiState { connected: true, ..Default::default() };
    let live = ui.global::<Live>();
    live.set_connected(true);
    apply_status(&ui, &mut st, &s, &axis_indices(cfg));
    refresh_pills(&ui, &st, modules, &axis_indices(cfg));
    live.set_clock(chrono::Local::now().format("%H:%M").to_string().into());
    live.set_message("Simulated machine".into());

    let handle = linuxcnc::spawn(Box::new(linuxcnc::Sim::new()), Duration::from_secs(3600), |_| {});
    let manual = manual::wire(&ui, &handle, cfg, settings::Settings::memory(), axis_indices(cfg).contains(&3));
    mdi::wire(&ui, &handle, cfg, &mut Vec::new());
    files::wire(&ui, &handle, sample_dir());
    program::wire(&ui);
    program::sync(&ui, &s.file);
    let machine_cfg = machine_cfg::MachineCfg::load(cfg, &mut Vec::new());
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
            "line" => {
                live.set_line(18);
                live.set_running(true);
            }
            "file" => actions.invoke_files_tap(1),
            // PROBE state: probe-mode:<m>, probe-shape:<s>, probe-target:<t>.
            w if w.starts_with("probe-") => {
                if let Some((k, v)) = w.split_once(':') {
                    match k {
                        "probe-mode" => actions.invoke_probe_mode(v.into()),
                        "probe-shape" => actions.invoke_probe_shape(v.into()),
                        "probe-target" => actions.invoke_probe_target(v.into()),
                        _ => {}
                    }
                }
            }
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
                    manual::log_message(&ui, &manual, &linuxcnc::Message { kind, text: text.into() });
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
    /// The status handler lives on the UI thread; the machine thread only
    /// posts events to it.
    static UI_EVENT: RefCell<Option<UiEventFn>> = RefCell::new(None);
    static MANUAL: RefCell<Option<Rc<RefCell<manual::Manual>>>> = RefCell::new(None);
}

thread_local! {
    static TOAST: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Show a refusal over the bottom bar for a few seconds.
fn toast(ui: &MainWindow, text: String) {
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
