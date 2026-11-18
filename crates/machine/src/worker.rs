//! The thread that owns the machine connection.
//!
//! It polls status at a fixed period, reports a status only when it changed,
//! forwards operator messages, and runs commands from a queue so the UI thread
//! never blocks on LinuxCNC.

use crate::{Interp, Machine, Mode, Status};
use lcnc::{Command, JogTarget, JointSel, Message, MessageKind, Serial, TrajMode, Wcs, JOINTS};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::RwLock;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Event {
    Status(Box<Status>),
    Message(Message),
    /// The connection failed; the worker keeps retrying the poll, and the
    /// next Status means it is back.
    Disconnected(String),
}

/// What the worker takes from its queue.
enum Request {
    Cmd(Command),
    SimToolChanged,
}

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Request>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(Request::Cmd(cmd));
    }

    /// Simulator only: the operator finished the pending M6.
    pub fn sim_tool_changed(&self) {
        let _ = self.tx.send(Request::SimToolChanged);
    }

    /// A handle with no machine behind it (tests): commands are dropped.
    pub fn detached() -> Self {
        Self { tx: mpsc::channel().0 }
    }
}

pub fn spawn<F>(mut machine: Box<dyn Machine>, period: Duration, mut on_event: F) -> Handle
where
    F: FnMut(Event) + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("machine".into())
        .spawn(move || run(&mut *machine, &rx, period, &mut on_event))
        .expect("spawn machine thread");
    Handle { tx }
}

fn run(m: &mut dyn Machine, rx: &Receiver<Request>, period: Duration, on_event: &mut dyn FnMut(Event)) {
    // The last status reported, and the simulator's tool change with it.
    let mut last: Option<lcnc::Status> = None;
    let mut last_tool: Option<i32> = None;
    let mut connected = true;
    // Report a lost connection only after it stays lost for a second; the
    // first polls after connecting can come before task publishes status.
    let mut failing_since: Option<Instant> = None;
    let mut back: Option<MdiReturn> = None;
    let mut modes: Option<ModeRestore> = None;
    loop {
        let start = Instant::now();
        loop {
            match rx.try_recv() {
                Ok(Request::SimToolChanged) => m.sim_tool_changed(),
                Ok(Request::Cmd(cmd)) => {
                    let is_mdi = matches!(cmd, Command::Mdi(_));
                    // Put modes back from a finished macro before anything
                    // that reads them: the next MDI line, or a program.
                    if matches!(cmd, Command::Mdi(_) | Command::ProgramRun(_) | Command::Step) {
                        if let Some(sent) = flush_modes(m, &last, &mut modes, on_event) {
                            back = after_mdi(back.take(), Some(sent));
                        }
                    }
                    if matches!(cmd, Command::ProgramRun(_) | Command::Step) {
                        start_modes(m, &last, on_event);
                    }
                    let call = match &cmd {
                        Command::Mdi(line) if restores_modes(line) => true,
                        _ => false,
                    };
                    let snap = if call && modes.is_none() { m.poll().ok().filter(|s| !s.busy() && s.queued_mdi == 0).cloned() } else { None };
                    let picks_mode = required_mode(&cmd).is_some() || matches!(cmd, Command::SetMode(_));
                    let sent = execute(m, &last, cmd, on_event);
                    if call {
                        modes = after_macro(modes.take(), snap, sent);
                    }
                    if is_mdi {
                        back = after_mdi(back.take(), sent);
                    } else if picks_mode {
                        back = None;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // Decide from the borrowed status; act (send, report) after.
        let tool = m.sim_tool_change();
        let mut flush = false;
        let mut due = None;
        let mut changed = None;
        match m.poll() {
            Ok(s) => {
                connected = true;
                failing_since = None;
                flush = modes.as_ref().is_some_and(|r| r.done(s));
                if back.as_ref().is_some_and(|b| b.overtaken(s)) {
                    // Someone else (the pendant, halui) changed mode after
                    // the MDI command: follow them, don't switch back later.
                    back = None;
                }
                // With a restore pending, the mode goes back after it.
                if modes.is_none() {
                    due = back.as_mut().and_then(|b| b.due(s));
                }
                if last.as_ref() != Some(s) || last_tool != tool {
                    changed = Some(s.clone());
                }
            }
            Err(e) => {
                let since = *failing_since.get_or_insert_with(Instant::now);
                if connected && since.elapsed() >= Duration::from_secs(1) {
                    on_event(Event::Disconnected(e.to_string()));
                    connected = false;
                }
            }
        }
        if let Some(s) = changed {
            on_event(Event::Status(Box::new(Status { lcnc: s.clone(), sim_tool_change: tool })));
            last = Some(s);
            last_tool = tool;
        }
        if flush {
            if let Some(sent) = flush_modes(m, &last, &mut modes, on_event) {
                back = after_mdi(back.take(), Some(sent));
            }
        } else if let Some(mode) = due {
            back = None;
            let _ = m.send(&Command::SetMode(mode));
        }
        while let Some(msg) = m.next_message() {
            on_event(Event::Message(msg));
        }
        if let Some(rest) = period.checked_sub(start.elapsed()) {
            thread::sleep(rest);
        }
    }
}

/// After an MDI command the mode goes back to what it was, as Probe Basic
/// does (QtPyVCP `issue_mdi`, and the mill's own MDI workers). Otherwise
/// task stays in MDI with motion in coordinated mode, where the pendant's
/// wheel (axis.N.jog-counts, teleop only) does nothing.
struct MdiReturn {
    mode: Mode,
    /// The last MDI command sent; nothing goes back before task echoes it.
    serial: Serial,
    idle_since: Option<Instant>,
}

/// How long MDI must stay idle before the switch back, so a run of MDI
/// lines (a probe routine sends one at a time) doesn't flip modes between
/// lines.
const MDI_RETURN_AFTER: Duration = Duration::from_millis(500);

fn after_mdi(back: Option<MdiReturn>, sent: Option<(Mode, Serial)>) -> Option<MdiReturn> {
    let (before, serial) = sent?;
    match back {
        Some(b) => Some(MdiReturn { serial, idle_since: None, ..b }),
        None if before != Mode::Mdi => Some(MdiReturn { mode: before, serial, idle_since: None }),
        None => None,
    }
}

impl MdiReturn {
    /// Task left MDI after it took the command: another program (the
    /// pendant asking for MANUAL to jog) changed mode.
    fn overtaken(&self, s: &lcnc::Status) -> bool {
        s.mode != Mode::Mdi && s.echo_serial >= self.serial
    }

    /// The mode to go back to, once MDI has finished and stayed idle.
    fn due(&mut self, s: &lcnc::Status) -> Option<Mode> {
        if s.mode != Mode::Mdi {
            // Someone else (the pendant, a program run) changed mode.
            self.idle_since = None;
            return None;
        }
        let done = s.finished(self.serial) && s.interp == Interp::Idle && s.queued_mdi == 0;
        if !done {
            self.idle_since = None;
            return None;
        }
        (self.idle_since.get_or_insert_with(Instant::now).elapsed() >= MDI_RETURN_AFTER).then_some(self.mode)
    }
}

/// Modes a macro changed are put back once it ends, finished or stopped.
///
/// Stock probe macros end in G91 (bore, pocket, slot), and a macro stopped
/// partway (STOP, E-stop, a probe error) leaves whatever it had set: G91, a
/// work system, units, the tool offset off. LinuxCNC's abort only puts back
/// G90 and G40 (on_abort.ngc), and those modes carry into MDI and the next
/// program. So millgui notes the modes before each `o<...> call` and, once
/// the call is over, puts back the ones that differ. Typed G-codes (G91,
/// G55) are left as typed; only macro calls are wrapped.
struct ModeRestore {
    before: Modes,
    /// The last macro call sent; nothing is restored before task echoes it.
    serial: Serial,
}

/// The modes a restore puts back.
#[derive(Debug, Clone, PartialEq)]
struct Modes {
    units: Option<i32>,
    plane: Option<i32>,
    feed: Option<i32>,
    wcs: Wcs,
    distance: Option<i32>,
    /// G43 on (from the tool table), and the tool it was for.
    tool_offset: bool,
    tool: i32,
}

impl Modes {
    fn of(s: &lcnc::Status) -> Self {
        // Codes are stored as code * 10, in fixed slots; look them up by
        // value so the slot layout doesn't matter.
        let pick = |set: &[i32]| s.active_gcodes[1..].iter().copied().find(|g| set.contains(g));
        Modes {
            units: pick(&[200, 210]),
            plane: pick(&[170, 180, 190]),
            feed: pick(&[930, 940, 950]),
            wcs: s.wcs,
            distance: pick(&[900, 910]),
            tool_offset: s.active_gcodes[1..].contains(&430),
            tool: s.tool_in_spindle,
        }
    }

    /// The MDI line that takes `now` back to these modes, if any differ.
    fn line_from(&self, now: &Modes) -> Option<String> {
        let g = |c: i32| if c % 10 == 0 { format!("G{}", c / 10) } else { format!("G{}.{}", c / 10, c % 10) };
        let mut w = Vec::new();
        for (was, is) in [(self.units, now.units), (self.plane, now.plane), (self.feed, now.feed)] {
            if let Some(c) = was.filter(|c| Some(*c) != is) {
                w.push(g(c));
            }
        }
        if self.wcs != now.wcs && self.wcs.index().is_some() {
            w.push(self.wcs.name().to_string());
        }
        if let Some(c) = self.distance.filter(|c| Some(*c) != now.distance) {
            w.push(g(c));
        }
        // Only an offset the macro turned off is turned back on, and only
        // for the same tool: a touch-off ends with a new G43 on purpose, and
        // a tool change brings its own.
        if self.tool_offset && !now.tool_offset && self.tool == now.tool && self.tool > 0 {
            w.push("G43".into());
        }
        (!w.is_empty()).then(|| w.join(" "))
    }
}

/// Marks an MDI line whose mode changes millgui puts back once it ends, as
/// it does for a macro call: a power feed move written as `G91 G1 X10`, a
/// park move with `G90 G53`.
pub const KEEP_MODES: &str = "(millgui: modes back after)";

/// `line` with the KEEP_MODES mark.
pub fn keep_modes(line: &str) -> String {
    format!("{line} {KEEP_MODES}")
}

/// Lines whose mode changes are put back afterwards: macro calls and
/// lines marked with KEEP_MODES.
fn restores_modes(line: &str) -> bool {
    is_macro_call(line) || line.contains(KEEP_MODES)
}

fn is_macro_call(line: &str) -> bool {
    let l = line.trim().to_ascii_lowercase();
    l.starts_with("o<") && l.split_once('>').is_some_and(|(_, rest)| rest.trim_start().starts_with("call"))
}

/// Start (or extend) a restore for a macro call that was sent. The modes
/// from before the first of a run of calls are the ones that come back.
fn after_macro(r: Option<ModeRestore>, snap: Option<lcnc::Status>, sent: Option<(Mode, Serial)>) -> Option<ModeRestore> {
    let Some((_, serial)) = sent else { return r };
    match r {
        Some(r) => Some(ModeRestore { serial, ..r }),
        None => snap.map(|s| ModeRestore { before: Modes::of(&s), serial }),
    }
}

impl ModeRestore {
    /// The call is over: echoed, task no longer executing it, interpreter
    /// idle. Holds after a stop or E-stop too.
    fn done(&self, s: &lcnc::Status) -> bool {
        s.finished(self.serial) && s.interp == Interp::Idle && s.queued_mdi == 0
    }
}

/// Send the restore line if the macro is over and LinuxCNC can take MDI,
/// and wait for it to finish (a following switch to AUTO would abort it).
/// After an E-stop it stays pending until the machine is on and homed.
/// Returns the mode and serial of the restore line, when one was sent.
fn flush_modes(m: &mut dyn Machine, last: &Option<lcnc::Status>, modes: &mut Option<ModeRestore>, on_event: &mut dyn FnMut(Event)) -> Option<(Mode, Serial)> {
    let s = fresh(m, last)?;
    let r = modes.as_ref()?;
    if !r.done(&s) {
        return None;
    }
    let Some(line) = r.before.line_from(&Modes::of(&s)) else {
        *modes = None;
        return None;
    };
    if !(s.power_on() && s.all_homed()) {
        return None;
    }
    *modes = None;
    let sent = execute(m, last, Command::Mdi(line.clone()), on_event)?;
    wait_for(m, Duration::from_secs(2), |s| s.finished(sent.1) && s.interp == Interp::Idle);
    on_event(Event::Message(Message { kind: MessageKind::Text, text: format!("Modes put back: {line}") }));
    Some(sent)
}

/// The words a program start needs that the current modes lack, so a
/// program runs the same after MDI or a macro as after a fresh start
/// (RS274NGC_STARTUP_CODE): absolute moves, XY plane, no cutter comp, no
/// canned cycle, feed per minute, incremental arc centres. Units, the work
/// system and the tool offset are the operator's and stay as they are.
fn start_words(s: &lcnc::Status) -> Option<String> {
    let g = &s.active_gcodes[1..];
    let has = |c: i32| g.contains(&c);
    let mut w = Vec::new();
    if (810..=890).contains(&s.active_gcodes[1]) || (730..=760).contains(&s.active_gcodes[1]) {
        w.push("G80");
    }
    if has(180) || has(190) {
        w.push("G17");
    }
    if has(410) || has(420) {
        w.push("G40");
    }
    if has(930) || has(950) {
        w.push("G94");
    }
    if has(910) {
        w.push("G90");
    }
    if has(901) {
        w.push("G91.1");
    }
    (!w.is_empty()).then(|| w.join(" "))
}

/// Before CYCLE START (or a first single step) from idle, set the modes a
/// program expects, in MDI, and wait for it: the switch to AUTO that
/// follows would abort it.
fn start_modes(m: &mut dyn Machine, last: &Option<lcnc::Status>, on_event: &mut dyn FnMut(Event)) {
    let Some(s) = fresh(m, last) else { return };
    if s.busy() || !(s.power_on() && s.all_homed()) {
        return;
    }
    let Some(line) = start_words(&s) else { return };
    if let Some((_, serial)) = execute(m, last, Command::Mdi(line.clone()), on_event) {
        wait_for(m, Duration::from_secs(2), |s| s.finished(serial) && s.interp == Interp::Idle);
        on_event(Event::Message(Message { kind: MessageKind::Text, text: format!("Set {line} for the program start") }));
    }
}

/// Commands that need a task mode get it switched first, the way LinuxCNC's
/// own GUIs do. A switch is only attempted while idle; otherwise LinuxCNC's
/// own error is reported.
fn required_mode(cmd: &Command) -> Option<Mode> {
    match cmd {
        Command::Mdi(_) => Some(Mode::Mdi),
        // Loading needs no mode (task opens a file in any idle mode), and
        // Probe Basic doesn't switch for it; a switch to AUTO aborts and
        // resynchs the interpreter.
        Command::ProgramRun(_) | Command::Step => Some(Mode::Auto),
        Command::JogCont { .. }
        | Command::JogIncr { .. }
        | Command::Home(_)
        | Command::Unhome(_)
        | Command::SpindleOn { .. }
        | Command::SpindleOff => {
            Some(Mode::Manual)
        }
        _ => None,
    }
}

/// What the operator asked for, for "didn't happen" messages.
fn describe(cmd: &Command) -> &'static str {
    match cmd {
        Command::Mdi(_) => "MDI command",
        Command::ProgramRun(_) => "Program start",
        Command::ProgramOpen(_) => "Program load",
        Command::Step => "Single step",
        Command::JogCont { .. } | Command::JogIncr { .. } => "Jog",
        Command::Home(_) => "Homing",
        Command::Unhome(_) => "Unhome",
        Command::SpindleOn { .. } => "Spindle start",
        Command::SpindleOff => "Spindle stop",
        _ => "Command",
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Manual => "MANUAL",
        Mode::Auto => "AUTO",
        Mode::Mdi => "MDI",
        Mode::Unknown(_) => "an unknown",
    }
}

/// Returns the mode task was in before the command and the command's serial,
/// once it was sent.
fn execute(m: &mut dyn Machine, last: &Option<lcnc::Status>, cmd: Command, on_event: &mut dyn FnMut(Event)) -> Option<(Mode, Serial)> {
    // Decide from a fresh status, not the last periodic poll: an earlier
    // command in the same batch may have switched mode or started motion,
    // and a mode switch while something runs aborts it (task's
    // emcTaskSetMode aborts motion and the interpreter).
    let now = fresh(m, last);
    if let (Some(want), Some(s)) = (required_mode(&cmd), &now) {
        if s.mode != want && !s.busy() {
            if let Err(e) = m.send(&Command::SetMode(want)) {
                on_event(Event::Message(Message { kind: MessageKind::Error, text: e.to_string() }));
                return None;
            }
            wait_for(m, Duration::from_millis(500), |s| s.mode == want);
            // Task refuses some switches (while jogging it only says so) and
            // then takes the command in the old mode, where a program run is
            // dropped without a word. Stop here and say what didn't happen;
            // a spindle stop is still sent, whatever the mode.
            if cmd != Command::SpindleOff && m.poll().is_ok_and(|s| s.mode != want) {
                let mut why = None;
                while let Some(msg) = m.next_message() {
                    if msg.kind == MessageKind::Error {
                        why = Some(msg.text.trim().to_string());
                    }
                    on_event(Event::Message(msg));
                }
                let text = format!(
                    "{} didn't happen: LinuxCNC stayed in {} mode{}",
                    describe(&cmd),
                    mode_name(s.mode),
                    why.map(|w| format!(" ({w})")).unwrap_or_default()
                );
                on_event(Event::Message(Message { kind: MessageKind::Error, text }));
                return None;
            }
        }
    }
    let last = &now;
    // Jogging: teleop (axis jogs) when homed, free mode (joint jogs) when not.
    if let (Command::JogCont { .. } | Command::JogIncr { .. }, Some(s)) = (&cmd, fresh(m, last)) {
        let want = s.all_homed();
        if (s.traj_mode == TrajMode::Teleop) != want {
            let _ = m.send(&Command::Teleop(want));
            wait_for(m, Duration::from_millis(500), |s| (s.traj_mode == TrajMode::Teleop) == want);
        }
    }
    let teleop = fresh(m, last).is_some_and(|s| s.traj_mode == TrajMode::Teleop);
    let stop = matches!(cmd, Command::JogStop { .. });
    let Some(cmd) = jog_in_mode(cmd, teleop) else {
        if !stop {
            let text = "Home the machine first: this axis does not jog before homing".to_string();
            on_event(Event::Message(Message { kind: MessageKind::Error, text }));
        }
        return None;
    };
    // Homing: motion must be in free (joint) mode, as LinuxCNC's own GUIs
    // make sure before they home; homed machines sit in teleop.
    if let (Command::Home(_), Some(_)) = (&cmd, last) {
        if m.poll().is_ok_and(|s| s.traj_mode != TrajMode::Free) {
            let _ = m.send(&Command::Teleop(false));
            wait_for(m, Duration::from_millis(500), |s| s.traj_mode == TrajMode::Free);
        }
    }
    let before = now.as_ref().map_or(Mode::Manual, |s| s.mode);
    let serial = match m.send(&cmd) {
        Ok(serial) => serial,
        Err(e) => {
            on_event(Event::Message(Message { kind: MessageKind::Error, text: e.to_string() }));
            return None;
        }
    };
    // Say so when a home command was taken but nothing started: LinuxCNC
    // drops it without a message when motion is not enabled.
    if let Command::Home(j) = cmd {
        let started = |s: &lcnc::Status| {
            let n = s.joints.min(JOINTS);
            match j {
                JointSel::All => s.homing[..n].iter().any(|h| *h) || s.all_homed(),
                JointSel::Joint(j) => (j as usize) < n && (s.homing[j as usize] || s.homed[j as usize]),
            }
        };
        let mut seen = None;
        let end = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < end {
            if let Ok(s) = m.poll() {
                if started(s) {
                    return None;
                }
                seen = Some(s.clone());
            }
            while let Some(msg) = m.next_message() {
                on_event(Event::Message(msg));
            }
            thread::sleep(Duration::from_millis(20));
        }
        if let Some(s) = seen {
            // Short: the header shows one line (tap it for the log).
            let why = if !s.power_on() || !s.enabled {
                ": machine not on"
            } else if s.traj_mode != TrajMode::Free {
                ": not in joint mode"
            } else if s.mode != Mode::Manual {
                ": not in manual mode"
            } else {
                ""
            };
            let text = format!("Homing didn't start{why}");
            on_event(Event::Message(Message { kind: MessageKind::Error, text }));
        }
    }
    Some((before, serial))
}

fn wait_for(m: &mut dyn Machine, timeout: Duration, done: impl Fn(&lcnc::Status) -> bool) {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if m.poll().is_ok_and(&done) {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// A fresh status, or the last one when the poll fails.
fn fresh(m: &mut dyn Machine, last: &Option<lcnc::Status>) -> Option<lcnc::Status> {
    m.poll().ok().cloned().or_else(|| last.clone())
}

/// Which joint jogs each axis (by index, X=0 .. W=8) before homing; `None`
/// for an axis that has no single joint (a gantry's two Y joints, non-trivial
/// kinematics). Identity until [`set_axis_joints`].
static AXIS_JOINTS: RwLock<Option<[Option<u8>; 9]>> = RwLock::new(None);

/// Set the axis to joint map from the machine's kinematics.
pub fn set_axis_joints(map: [Option<u8>; 9]) {
    *AXIS_JOINTS.write().unwrap_or_else(|e| e.into_inner()) = Some(map);
}

fn axis_joint(axis: usize) -> Option<u8> {
    match *AXIS_JOINTS.read().unwrap_or_else(|e| e.into_inner()) {
        Some(map) => map.get(axis).copied().flatten(),
        None => Some(axis as u8),
    }
}

/// Screens jog axes. LinuxCNC 2.9 takes axis jogs only in teleop and joint
/// jogs only in free mode (before homing). `None` for a jog of an axis with
/// no single joint before homing.
fn jog_in_mode(cmd: Command, teleop: bool) -> Option<Command> {
    let joint = |t: JogTarget| match t {
        JogTarget::Axis(a) if !teleop => axis_joint(a.index()).map(JogTarget::Joint),
        t => Some(t),
    };
    Some(match cmd {
        Command::JogCont { target, vel } => Command::JogCont { target: joint(target)?, vel },
        Command::JogIncr { target, vel, incr } => Command::JogIncr { target: joint(target)?, vel, incr },
        Command::JogStop { target } => Command::JogStop { target: joint(target)? },
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sim;
    use lcnc::{RcsStatus, Result, TaskState};

    #[test]
    fn mdi_switches_to_mdi_mode_first() {
        let mut sim = Sim::new();
        sim.send(&Command::SetState(TaskState::EstopReset)).unwrap();
        sim.send(&Command::SetState(TaskState::On)).unwrap();
        let last = Some(sim.poll().unwrap().clone());
        execute(&mut sim, &last, Command::Mdi("G0 X0".into()), &mut |_| {});
        assert_eq!(sim.poll().unwrap().mode, Mode::Mdi);
    }

    /// LinuxCNC while an axis is still jogging: it says so and keeps the
    /// mode, then takes a program run in MANUAL and drops it.
    struct Jogging {
        sim: Sim,
        sent: Vec<Command>,
        said: bool,
    }

    impl Machine for Jogging {
        fn poll(&mut self) -> Result<&lcnc::Status> {
            self.sim.poll()
        }
        fn send(&mut self, cmd: &Command) -> Result<Serial> {
            self.sent.push(cmd.clone());
            if let Command::SetMode(_) = cmd {
                self.said = true;
                return Ok(Serial(0));
            }
            self.sim.send(cmd)
        }
        fn next_message(&mut self) -> Option<Message> {
            if std::mem::take(&mut self.said) {
                return Some(Message { kind: MessageKind::Error, text: "Ignoring task mode change while jogging".into() });
            }
            self.sim.next_message()
        }
    }

    fn on_jogging() -> Jogging {
        let mut sim = Sim::new();
        sim.send(&Command::SetState(TaskState::EstopReset)).unwrap();
        sim.send(&Command::SetState(TaskState::On)).unwrap();
        sim.send(&Command::Home(JointSel::All)).unwrap();
        Jogging { sim, sent: vec![], said: false }
    }

    #[test]
    fn refused_mode_switch_stops_the_command_and_says_why() {
        let mut m = on_jogging();
        let mut said = vec![];
        execute(&mut m, &None, Command::ProgramRun(0), &mut |e| {
            if let Event::Message(msg) = e {
                said.push(msg.text);
            }
        });
        assert!(!m.sent.contains(&Command::ProgramRun(0)), "sent {:?}", m.sent);
        assert_eq!(said.last().unwrap(), "Program start didn't happen: LinuxCNC stayed in MANUAL mode (Ignoring task mode change while jogging)");
    }

    #[test]
    fn spindle_stop_is_sent_whatever_the_mode() {
        let mut m = on_jogging();
        m.sim.send(&Command::SetMode(Mode::Mdi)).unwrap();
        execute(&mut m, &None, Command::SpindleOff, &mut |_| {});
        assert!(m.sent.contains(&Command::SpindleOff));
    }

    #[test]
    fn mode_comes_from_a_fresh_status_not_the_last_poll() {
        // The last poll said MDI, but an earlier command already switched
        // to AUTO: no second switch (which would abort what just started).
        let mut m = on_jogging();
        m.sim.send(&Command::SetMode(Mode::Auto)).unwrap();
        let mut stale = m.sim.poll().unwrap().clone();
        stale.mode = Mode::Mdi;
        execute(&mut m, &Some(stale), Command::ProgramRun(0), &mut |_| {});
        assert_eq!(m.sent, vec![Command::ProgramRun(0)]);
    }

    #[test]
    fn step_from_idle_switches_to_auto() {
        assert_eq!(required_mode(&Command::Step), Some(Mode::Auto));
    }

    fn mdi_idle(serial: i32) -> lcnc::Status {
        lcnc::Status { mode: Mode::Mdi, interp: Interp::Idle, echo_serial: Serial(serial), command_status: RcsStatus::Done, ..Default::default() }
    }

    #[test]
    fn mdi_goes_back_to_the_mode_it_came_from_once_idle() {
        let mut b = after_mdi(None, Some((Mode::Manual, Serial(7)))).unwrap();
        // Not echoed yet, then still executing, then queued: stay in MDI.
        assert_eq!(b.due(&mdi_idle(6)), None);
        assert_eq!(b.due(&lcnc::Status { command_status: RcsStatus::Exec, ..mdi_idle(7) }), None);
        assert_eq!(b.due(&lcnc::Status { queued_mdi: 1, ..mdi_idle(7) }), None);
        assert_eq!(b.due(&lcnc::Status { interp: Interp::Reading, ..mdi_idle(7) }), None);
        assert_eq!(b.due(&mdi_idle(7)), None, "not before it has been idle a while");
        b.idle_since = Some(Instant::now() - MDI_RETURN_AFTER);
        assert_eq!(b.due(&mdi_idle(7)), Some(Mode::Manual));
    }

    #[test]
    fn a_later_mdi_line_keeps_the_original_mode_and_waits_for_its_own_echo() {
        let b = after_mdi(None, Some((Mode::Auto, Serial(3))));
        let mut b = after_mdi(b, Some((Mode::Mdi, Serial(9)))).unwrap();
        assert_eq!(b.mode, Mode::Auto);
        b.idle_since = Some(Instant::now() - MDI_RETURN_AFTER);
        assert_eq!(b.due(&mdi_idle(8)), None);
    }

    #[test]
    fn a_mode_change_by_someone_else_cancels_the_return() {
        let b = after_mdi(None, Some((Mode::Manual, Serial(7)))).unwrap();
        let mut s = mdi_idle(7);
        assert!(!b.overtaken(&s));
        s.mode = Mode::Auto;
        assert!(b.overtaken(&s));
        // Before task has the command, the old mode is not someone else's.
        s.echo_serial = Serial(6);
        assert!(!b.overtaken(&s));
    }

    #[test]
    fn mdi_from_mdi_mode_stays_there() {
        assert!(after_mdi(None, Some((Mode::Mdi, Serial(4)))).is_none());
        assert!(after_mdi(None, None).is_none(), "nothing sent, nothing to undo");
    }

    #[test]
    fn home_leaves_teleop_first() {
        let mut sim = Sim::new();
        sim.send(&Command::SetState(TaskState::EstopReset)).unwrap();
        sim.send(&Command::SetState(TaskState::On)).unwrap();
        sim.send(&Command::Teleop(true)).unwrap();
        let last = Some(sim.poll().unwrap().clone());
        execute(&mut sim, &last, Command::Home(JointSel::All), &mut |_| {});
        let s = sim.poll().unwrap();
        assert_eq!(s.traj_mode, TrajMode::Free);
        assert!(s.all_homed());
    }

    fn modal(codes: &[i32], wcs: i32, tool: i32) -> lcnc::Status {
        let mut s = lcnc::Status::default();
        for (i, c) in codes.iter().enumerate() {
            s.active_gcodes[i + 1] = *c;
        }
        s.wcs = Wcs::from_raw(wcs);
        s.tool_in_spindle = tool;
        s
    }

    #[test]
    fn macro_calls_are_wrapped_typed_codes_are_not() {
        assert!(is_macro_call("o<probe_round_pocket_center_start> call"));
        assert!(is_macro_call("  O<mymill_powerfeed> CALL [1] [2]"));
        assert!(!is_macro_call("G91"));
        assert!(!is_macro_call("o100 if [#1 GT 0]"));
        assert!(!is_macro_call("o<x> sub"));
    }

    #[test]
    fn restore_puts_back_what_the_macro_changed() {
        let before = Modes::of(&modal(&[210, 170, 940, 900, 430], 1, 3));
        // Bore probe: ends in G91.
        assert_eq!(before.line_from(&Modes::of(&modal(&[210, 170, 940, 910, 430], 1, 3))).as_deref(), Some("G90"));
        // Stopped partway: G20, G55, G91, offset off.
        let now = Modes::of(&modal(&[200, 170, 940, 910, 490], 2, 3));
        assert_eq!(before.line_from(&now).as_deref(), Some("G21 G54 G90 G43"));
        assert_eq!(before.line_from(&before), None);
    }

    #[test]
    fn tool_offset_only_comes_back_for_the_same_tool() {
        let on = Modes::of(&modal(&[210, 900, 430], 1, 3));
        assert_eq!(on.line_from(&Modes::of(&modal(&[210, 900, 490], 1, 4))), None);
        // A touch-off that turns G43 on is left alone.
        let off = Modes::of(&modal(&[210, 900, 490], 1, 3));
        assert_eq!(off.line_from(&Modes::of(&modal(&[210, 900, 430], 1, 3))), None);
    }

    #[test]
    fn restore_waits_for_the_call_and_keeps_the_first_snapshot() {
        let first = modal(&[900], 1, 0);
        let r = after_macro(None, Some(first), Some((Mode::Manual, Serial(5)))).unwrap();
        let r = after_macro(Some(r), Some(modal(&[910], 2, 0)), Some((Mode::Mdi, Serial(6)))).unwrap();
        assert_eq!(r.serial, Serial(6));
        assert_eq!(r.before.wcs, Wcs::G54);
        let mut s = modal(&[910], 2, 0);
        s.echo_serial = Serial(5);
        assert!(!r.done(&s));
        s.echo_serial = Serial(6);
        s.command_status = RcsStatus::Exec;
        assert!(!r.done(&s));
        s.command_status = RcsStatus::Done;
        assert!(r.done(&s));
        // Not sent: nothing to restore.
        assert!(after_macro(None, Some(modal(&[900], 1, 0)), None).is_none());
    }

    #[test]
    fn program_start_sets_absolute_and_clears_leftovers() {
        assert_eq!(start_words(&modal(&[10, 170, 400, 210, 900, 940, 540, 911], 1, 0)), None);
        assert_eq!(start_words(&modal(&[10, 170, 400, 210, 910, 940, 540, 911], 1, 0)).as_deref(), Some("G90"));
        assert_eq!(start_words(&modal(&[810, 190, 410, 200, 910, 930, 550, 901], 2, 0)).as_deref(), Some("G80 G17 G40 G94 G90 G91.1"));
    }
}
