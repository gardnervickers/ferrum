//! The thread that owns the machine connection.
//!
//! It polls status at a fixed period, reports a status only when it changed,
//! forwards operator messages, and runs commands from a queue so the UI thread
//! never blocks on LinuxCNC.

use crate::status::*;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
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

#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
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

fn run(m: &mut dyn Machine, rx: &Receiver<Command>, period: Duration, on_event: &mut dyn FnMut(Event)) {
    let mut last: Option<Status> = None;
    let mut connected = true;
    // Report a lost connection only after it stays lost for a second; the
    // first polls after connecting can come before task publishes status.
    let mut failing_since: Option<Instant> = None;
    let mut back: Option<MdiReturn> = None;
    loop {
        let start = Instant::now();
        loop {
            match rx.try_recv() {
                Ok(cmd) => {
                    let is_mdi = matches!(cmd, Command::Mdi(_));
                    let picks_mode = required_mode(&cmd).is_some() || matches!(cmd, Command::SetMode(_));
                    let sent = execute(m, &last, cmd, on_event);
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
        match m.poll() {
            Ok(s) => {
                connected = true;
                failing_since = None;
                if let Some(mode) = back.as_mut().and_then(|b| b.due(&s)) {
                    back = None;
                    let _ = m.send(&Command::SetMode(mode));
                }
                if last.as_ref() != Some(&s) {
                    on_event(Event::Status(Box::new(s.clone())));
                    last = Some(s);
                }
            }
            Err(e) => {
                let since = *failing_since.get_or_insert_with(Instant::now);
                if connected && since.elapsed() >= Duration::from_secs(1) {
                    on_event(Event::Disconnected(e));
                    connected = false;
                }
            }
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
    serial: i32,
    idle_since: Option<Instant>,
}

/// How long MDI must stay idle before the switch back, so a run of MDI
/// lines (a probe routine sends one at a time) doesn't flip modes between
/// lines.
const MDI_RETURN_AFTER: Duration = Duration::from_millis(500);

fn after_mdi(back: Option<MdiReturn>, sent: Option<(Mode, i32)>) -> Option<MdiReturn> {
    let (before, serial) = sent?;
    match back {
        Some(b) => Some(MdiReturn { serial, idle_since: None, ..b }),
        None if before != Mode::Mdi => Some(MdiReturn { mode: before, serial, idle_since: None }),
        None => None,
    }
}

impl MdiReturn {
    /// The mode to go back to, once MDI has finished and stayed idle.
    fn due(&mut self, s: &Status) -> Option<Mode> {
        if s.mode != Mode::Mdi {
            // Someone else (the pendant, a program run) changed mode.
            self.idle_since = None;
            return None;
        }
        // RCS_EXEC (2) while task still works on the command.
        let done = s.echo_serial >= self.serial && s.command_status != 2 && s.interp == Interp::Idle && s.queued_mdi == 0;
        if !done {
            self.idle_since = None;
            return None;
        }
        (self.idle_since.get_or_insert_with(Instant::now).elapsed() >= MDI_RETURN_AFTER).then_some(self.mode)
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
        | Command::SpindleOn(_)
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
        Command::SpindleOn(_) => "Spindle start",
        Command::SpindleOff => "Spindle stop",
        _ => "Command",
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Manual => "MANUAL",
        Mode::Auto => "AUTO",
        Mode::Mdi => "MDI",
    }
}

/// Returns the mode task was in before the command and the command's serial,
/// once it was sent.
fn execute(m: &mut dyn Machine, last: &Option<Status>, cmd: Command, on_event: &mut dyn FnMut(Event)) -> Option<(Mode, i32)> {
    // Decide from a fresh status, not the last periodic poll: an earlier
    // command in the same batch may have switched mode or started motion,
    // and a mode switch while something runs aborts it (task's
    // emcTaskSetMode aborts motion and the interpreter).
    let now = m.poll().ok().or_else(|| last.clone());
    if let (Some(want), Some(s)) = (required_mode(&cmd), &now) {
        if s.mode != want && !s.busy() {
            if let Err(e) = m.send(&Command::SetMode(want)) {
                on_event(Event::Message(Message { kind: MessageKind::Error, text: e }));
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
    if let (Command::JogCont { .. } | Command::JogIncr { .. }, Some(s)) = (&cmd, m.poll().ok().or_else(|| last.clone())) {
        let want = s.all_homed();
        if (s.traj_mode == 3) != want {
            let _ = m.send(&Command::Teleop(want));
            wait_for(m, Duration::from_millis(500), |s| (s.traj_mode == 3) == want);
        }
    }
    // Homing: motion must be in free (joint) mode, as LinuxCNC's own GUIs
    // make sure before they home; homed machines sit in teleop.
    if let (Command::Home(_), Some(_)) = (&cmd, last) {
        if let Ok(s) = m.poll() {
            if s.traj_mode != 1 {
                let _ = m.send(&Command::Teleop(false));
                wait_for(m, Duration::from_millis(500), |s| s.traj_mode == 1);
            }
        }
    }
    let before = now.as_ref().map_or(Mode::Manual, |s| s.mode);
    let serial = match m.send(&cmd) {
        Ok(serial) => serial,
        Err(e) => {
            on_event(Event::Message(Message { kind: MessageKind::Error, text: e }));
            return None;
        }
    };
    // Say so when a home command was taken but nothing started: LinuxCNC
    // drops it without a message when motion is not enabled.
    if let Command::Home(j) = cmd {
        let started = |s: &Status| {
            let n = s.joints.min(JOINTS);
            if j < 0 { s.homing[..n].iter().any(|h| *h) || s.all_homed() } else { (j as usize) < n && (s.homing[j as usize] || s.homed[j as usize]) }
        };
        let mut seen = None;
        let end = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < end {
            if let Ok(s) = m.poll() {
                if started(&s) {
                    return None;
                }
                seen = Some(s);
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
            } else if s.traj_mode != 1 {
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

fn wait_for(m: &mut dyn Machine, timeout: Duration, done: impl Fn(&Status) -> bool) {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if let Ok(s) = m.poll() {
            if done(&s) {
                return;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sim;

    #[test]
    fn mdi_switches_to_mdi_mode_first() {
        let mut sim = Sim::new();
        sim.send(&Command::SetState(TaskState::EstopReset)).unwrap();
        sim.send(&Command::SetState(TaskState::On)).unwrap();
        let last = Some(sim.poll().unwrap());
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
        fn poll(&mut self) -> Result<Status, String> {
            self.sim.poll()
        }
        fn send(&mut self, cmd: &Command) -> Result<i32, String> {
            self.sent.push(cmd.clone());
            if let Command::SetMode(_) = cmd {
                self.said = true;
                return Ok(0);
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
        sim.send(&Command::Home(-1)).unwrap();
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
        let mut stale = m.sim.poll().unwrap();
        stale.mode = Mode::Mdi;
        execute(&mut m, &Some(stale), Command::ProgramRun(0), &mut |_| {});
        assert_eq!(m.sent, vec![Command::ProgramRun(0)]);
    }

    #[test]
    fn step_from_idle_switches_to_auto() {
        assert_eq!(required_mode(&Command::Step), Some(Mode::Auto));
    }

    fn mdi_idle(serial: i32) -> Status {
        Status { mode: Mode::Mdi, interp: Interp::Idle, echo_serial: serial, command_status: 1, ..Status::default() }
    }

    #[test]
    fn mdi_goes_back_to_the_mode_it_came_from_once_idle() {
        let mut b = after_mdi(None, Some((Mode::Manual, 7))).unwrap();
        // Not echoed yet, then still executing, then queued: stay in MDI.
        assert_eq!(b.due(&mdi_idle(6)), None);
        assert_eq!(b.due(&Status { command_status: 2, ..mdi_idle(7) }), None);
        assert_eq!(b.due(&Status { queued_mdi: 1, ..mdi_idle(7) }), None);
        assert_eq!(b.due(&Status { interp: Interp::Reading, ..mdi_idle(7) }), None);
        assert_eq!(b.due(&mdi_idle(7)), None, "not before it has been idle a while");
        b.idle_since = Some(Instant::now() - MDI_RETURN_AFTER);
        assert_eq!(b.due(&mdi_idle(7)), Some(Mode::Manual));
    }

    #[test]
    fn a_later_mdi_line_keeps_the_original_mode_and_waits_for_its_own_echo() {
        let b = after_mdi(None, Some((Mode::Auto, 3)));
        let mut b = after_mdi(b, Some((Mode::Mdi, 9))).unwrap();
        assert_eq!(b.mode, Mode::Auto);
        b.idle_since = Some(Instant::now() - MDI_RETURN_AFTER);
        assert_eq!(b.due(&mdi_idle(8)), None);
    }

    #[test]
    fn mdi_from_mdi_mode_stays_there() {
        assert!(after_mdi(None, Some((Mode::Mdi, 4))).is_none());
        assert!(after_mdi(None, None).is_none(), "nothing sent, nothing to undo");
    }

    #[test]
    fn home_leaves_teleop_first() {
        let mut sim = Sim::new();
        sim.send(&Command::SetState(TaskState::EstopReset)).unwrap();
        sim.send(&Command::SetState(TaskState::On)).unwrap();
        sim.send(&Command::Teleop(true)).unwrap();
        let last = Some(sim.poll().unwrap());
        execute(&mut sim, &last, Command::Home(-1), &mut |_| {});
        let s = sim.poll().unwrap();
        assert_eq!(s.traj_mode, 1);
        assert!(s.all_homed());
    }
}
