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
    loop {
        let start = Instant::now();
        loop {
            match rx.try_recv() {
                Ok(cmd) => execute(m, &last, cmd, on_event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        match m.poll() {
            Ok(s) => {
                connected = true;
                failing_since = None;
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

/// Commands that need a task mode get it switched first, the way LinuxCNC's
/// own GUIs do. A switch is only attempted while idle; otherwise LinuxCNC's
/// own error is reported.
fn required_mode(cmd: &Command) -> Option<Mode> {
    match cmd {
        Command::Mdi(_) => Some(Mode::Mdi),
        Command::ProgramRun(_) | Command::ProgramOpen(_) => Some(Mode::Auto),
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

fn execute(m: &mut dyn Machine, last: &Option<Status>, cmd: Command, on_event: &mut dyn FnMut(Event)) {
    if let (Some(want), Some(s)) = (required_mode(&cmd), last) {
        if s.mode != want && !s.busy() {
            if let Err(e) = m.send(&Command::SetMode(want)) {
                on_event(Event::Message(Message { kind: MessageKind::Error, text: e }));
                return;
            }
            wait_for(m, Duration::from_millis(500), |s| s.mode == want);
        }
    }
    // Jogging: teleop (axis jogs) when homed, free mode (joint jogs) when not.
    if let (Command::JogCont { .. } | Command::JogIncr { .. }, Some(s)) = (&cmd, last) {
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
    if let Err(e) = m.send(&cmd) {
        on_event(Event::Message(Message { kind: MessageKind::Error, text: e }));
        return;
    }
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
                    return;
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
