//! The start-up splash, and `millgui --boot`, which starts LinuxCNC itself
//! so the splash covers the whole start.
//!
//! LinuxCNC starts its display last, after realtime, HAL and task are up.
//! Started that way (the usual `linuxcnc millgui.ini`), millgui's splash
//! only covers its own connection. `millgui --boot millgui.ini` turns it
//! round: millgui opens its window at once, runs `linuxcnc -v` underneath
//! and shows the steps LinuxCNC reports. When LinuxCNC reaches its DISPLAY
//! step it starts `millgui/run` as usual; that copy sees `MILLGUI_BOOT`,
//! tells the launcher it is there and then just waits, holding LinuxCNC's
//! session open, until the launcher exits ([`attach_if_launched`]).
//!
//! The splash never holds anything up: millgui connects and wires itself
//! while it shows, and touches go through as soon as the first status is in.

use crate::config::Config;
use crate::{Boot, BootStep, Brand, MainWindow};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

const STEPS: [&str; 4] = ["REALTIME", "HAL", "MOTION", "SCREEN"];
/// Where each step's share of the bar starts; HAL files share theirs.
const AT: [f32; 5] = [0.0, 0.14, 0.72, 0.84, 1.0];

/// The name and version for the marks (see ui/brand.slint).
pub fn init_brand(ui: &MainWindow) {
    let b = ui.global::<Brand>();
    b.set_version(env!("CARGO_PKG_VERSION").into());
    let letters: Vec<SharedString> = b.get_name().chars().map(|c| c.to_string().into()).collect();
    b.set_letters(ModelRc::new(VecModel::from(letters)));
}

/// Show the splash. `started` is how many steps LinuxCNC has already done
/// (3 when it started millgui as its display: only the SCREEN step is left).
pub fn show(ui: &MainWindow, started: usize, footer: String) {
    let boot = ui.global::<Boot>();
    boot.set_steps(ModelRc::new(VecModel::from(STEPS.iter().map(|l| BootStep { label: (*l).into(), state: "todo".into() }).collect::<Vec<_>>())));
    boot.set_footer(footer.into());
    boot.set_phase("boot".into());
    step(ui, started);
    let (doing, detail) = match started {
        0 => ("STARTING LINUXCNC", String::new()),
        _ => ("CONNECTING", "LinuxCNC is up; millgui is connecting to it".into()),
    };
    say(ui, doing, &detail);
    boot.set_progress(AT[started.min(3)] + 0.06 * (started >= 3) as i32 as f32);
}

/// The splash's bottom line: "LinuxCNC 2.9.10 · millgui.ini".
pub fn footer(version: Option<&str>, ini: Option<&Path>) -> String {
    let ini = ini.and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
    match (version, ini) {
        (Some(v), Some(i)) => format!("LinuxCNC {v}  ·  {i}"),
        (Some(v), None) => format!("LinuxCNC {v}"),
        (None, Some(i)) => i,
        (None, None) => String::new(),
    }
}

/// Mark step `n` as under way and the ones before it as done.
fn step(ui: &MainWindow, n: usize) {
    let steps = ui.global::<Boot>().get_steps();
    for i in 0..steps.row_count() {
        let mut s = steps.row_data(i).unwrap();
        let want = if i < n { "done" } else if i == n { "now" } else { "todo" };
        if s.state != want {
            s.state = want.into();
            steps.set_row_data(i, s);
        }
    }
}

fn fail_step(ui: &MainWindow) {
    let steps = ui.global::<Boot>().get_steps();
    for i in 0..steps.row_count() {
        let mut s = steps.row_data(i).unwrap();
        if s.state == "now" {
            s.state = "bad".into();
            steps.set_row_data(i, s);
        }
    }
}

fn say(ui: &MainWindow, doing: &str, detail: &str) {
    let boot = ui.global::<Boot>();
    boot.set_doing(doing.into());
    boot.set_detail(detail.into());
}

/// Connected and the first status is in: all steps done, fade out.
pub fn ready(ui: &MainWindow) {
    let boot = ui.global::<Boot>();
    if boot.get_phase() != "boot" {
        return;
    }
    step(ui, STEPS.len());
    boot.set_progress(1.0);
    say(ui, "READY", "");
    boot.set_phase("ready".into());
}

/// LinuxCNC stopped by itself while millgui ran (under --boot).
pub fn stopped(ui: &MainWindow) {
    let boot = ui.global::<Boot>();
    show(ui, 3, boot.get_footer().to_string());
    fail_step(ui);
    say(ui, "LINUXCNC STOPPED", "");
    boot.set_error("LinuxCNC stopped on its own while the screen was running, so nothing is controlling the machine now. Check the machine, then start again.".into());
    #[cfg(unix)]
    boot.set_log(launch::log_path().display().to_string().into());
    boot.set_can_retry(false);
    boot.set_phase("failed".into());
}

/// `--show boot-…` (headless screenshots): the splash at a fixed moment.
pub fn demo(ui: &MainWindow, what: &str) {
    let boot = ui.global::<Boot>();
    let footer = "LinuxCNC 2.9.10  ·  millgui.ini".to_string();
    match what {
        "boot" | "boot-start" => {
            show(ui, 0, footer);
            say(ui, "STARTING REALTIME", "Real-time OS, RTAPI and HAL");
            boot.set_progress(0.08);
            boot.set_seconds(1);
        }
        "boot-hal" => {
            show(ui, 0, footer);
            step(ui, 1);
            say(ui, "LOADING HAL", "mymill.hal   ·   1 of 3");
            boot.set_progress(AT[1]);
            boot.set_seconds(3);
        }
        "boot-connect" => {
            show(ui, 3, footer);
            boot.set_seconds(6);
        }
        "boot-fail" => {
            show(ui, 0, footer);
            step(ui, 1);
            boot.set_progress(AT[1] + 0.2);
            fail_step(ui);
            say(ui, "LINUXCNC DID NOT START", "");
            boot.set_error("hm2_eth: ERROR: receive packet: Resource temporarily unavailable\nhm2_eth: Hostmot2 board not found at 10.10.10.10\n./mymill.hal:7: waitpid failed /usr/bin/rtapi_app hm2_eth\n./mymill.hal:7: /usr/bin/rtapi_app exited without becoming ready".into());
            boot.set_log("/home/cnc/.cache/millgui/linuxcnc-start.log".into());
            boot.set_can_retry(true);
            boot.set_phase("failed".into());
        }
        "boot-lock" => {
            show(ui, 0, footer);
            say(ui, "LINUXCNC IS ALREADY RUNNING", "");
            boot.set_error("Another LinuxCNC session is running, or the last one did not shut down cleanly. Restarting stops that session first.".into());
            boot.set_can_retry(true);
            boot.set_phase("lock".into());
        }
        "boot-stop" => {
            show(ui, 3, footer);
            say(ui, "STOPPING LINUXCNC", "");
            boot.set_phase("stopping".into());
        }
        _ => {}
    }
}

/// The user is leaving: EXIT on the splash or in the app, closing the
/// window, SIGTERM/Ctrl-C. The start-up task sees it, stops things in order
/// and only then ends the event loop.
pub static QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set while `--boot` stops LinuxCNC (see main's signal handler).
pub static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// LinuxCNC runs under this process (`--boot`), and a signal came in;
/// main's watch timer decides what it meant.
pub static LAUNCHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static SIGNALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_quit() {
    QUIT.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn quitting() -> bool {
    QUIT.load(std::sync::atomic::Ordering::SeqCst)
}

/// How a wait on the splash ended.
pub enum Wait<T> {
    Done(T),
    /// EXIT on the splash, or LinuxCNC asked the display to stop.
    Quit,
}

/// Call `tick` every `period` on the UI thread until it returns a value.
/// The event loop keeps running (and drawing) meanwhile.
async fn every<T: 'static>(period: Duration, mut tick: impl FnMut() -> Option<T> + 'static) -> T {
    struct Slot<T> {
        value: Option<T>,
        waker: Option<Waker>,
    }
    let slot = Rc::new(RefCell::new(Slot { value: None, waker: None }));
    let timer = slint::Timer::default();
    {
        let slot = slot.clone();
        timer.start(slint::TimerMode::Repeated, period, move || {
            if slot.borrow().value.is_some() {
                return;
            }
            if let Some(v) = tick() {
                let mut s = slot.borrow_mut();
                s.value = Some(v);
                if let Some(w) = s.waker.take() {
                    w.wake();
                }
            }
        });
    }
    std::future::poll_fn(|cx| {
        let mut s = slot.borrow_mut();
        match s.value.take() {
            Some(v) => Poll::Ready(v),
            None => {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    })
    .await
}

/// Wait with the splash up until `poll` returns a value or the user leaves.
/// `poll` runs every 50 ms; the seconds counter ticks.
pub async fn wait_until<T: 'static>(ui: &MainWindow, mut poll: impl FnMut(&MainWindow) -> Option<T> + 'static) -> Wait<T> {
    let t0 = Instant::now();
    let base = ui.global::<Boot>().get_seconds();
    let weak = ui.as_weak();
    let mut first = true;
    every(Duration::from_millis(50), move || {
        let ui = weak.upgrade()?;
        // The first tick only lets the window draw once.
        if std::mem::take(&mut first) {
            return None;
        }
        let boot = ui.global::<Boot>();
        let s = base + t0.elapsed().as_secs() as i32;
        if boot.get_seconds() != s {
            boot.set_seconds(s);
        }
        if quitting() {
            return Some(Wait::Quit);
        }
        poll(&ui).map(Wait::Done)
    })
    .await
}

/// Until the user leaves.
pub async fn until_quit() {
    every(Duration::from_millis(50), || quitting().then_some(())).await
}

/// Connect to LinuxCNC (or build the simulator) on a thread while the
/// splash shows. On failure the splash says so and waits for EXIT.
pub async fn connect<M: Send + 'static>(ui: &MainWindow, f: impl FnOnce() -> Result<M, String> + Send + 'static) -> Option<M> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    let got = wait_until(ui, move |_| rx.try_recv().ok()).await;
    match got {
        Wait::Done(Ok(m)) => {
            ui.global::<Boot>().set_progress(AT[3] + 0.11);
            Some(m)
        }
        Wait::Done(Err(e)) => {
            eprintln!("millgui: {e}");
            fail_step(ui);
            let boot = ui.global::<Boot>();
            say(ui, "NO ANSWER FROM LINUXCNC", "");
            boot.set_error(format!("millgui could not connect to LinuxCNC: {e}").into());
            boot.set_phase("failed".into());
            until_quit().await;
            None
        }
        Wait::Quit => None,
    }
}

/// The display copy LinuxCNC starts under `millgui --boot`: tell the
/// launcher, then hold LinuxCNC's session open until the launcher closes
/// the socket (it exits, or LinuxCNC is stopping), and exit.
#[cfg(unix)]
pub fn attach_if_launched() {
    use std::io::{Read, Write};
    let Some(path) = std::env::var_os("MILLGUI_BOOT") else { return };
    let Ok(mut s) = std::os::unix::net::UnixStream::connect(&path) else {
        eprintln!("millgui: MILLGUI_BOOT is set but no launcher answers; starting as the display");
        return;
    };
    let _ = s.write_all(b"attach\n");
    let mut sink = [0u8; 64];
    while matches!(s.read(&mut sink), Ok(n) if n > 0) {}
    std::process::exit(0);
}

#[cfg(not(unix))]
pub fn attach_if_launched() {}

#[cfg(unix)]
pub use launch::Launcher;

#[cfg(unix)]
mod launch {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{channel, Receiver};

    const LOCK: &str = "/tmp/linuxcnc.lock";

    /// LinuxCNC started by millgui; dropped (or [`Launcher::shutdown`]) stops it.
    pub struct Launcher {
        child: Child,
        // LinuxCNC's script asks on a terminal (it only prints its steps,
        // and only answers the lock question, when stdin is one).
        _pty: OwnedFd,
        sock: PathBuf,
        // The display copy's connection: closing it ends LinuxCNC.
        attached: Option<UnixStream>,
        pub version: Option<String>,
    }

    /// One start of LinuxCNC, read line by line.
    struct Run {
        launcher: Launcher,
        listener: UnixListener,
        lines: Receiver<(bool, String)>,
        log: Option<std::fs::File>,
        tail: VecDeque<String>,
        errors: Vec<String>,
        hal_files: usize,
        hal_seen: usize,
        last_error: bool,
        step: usize,
    }

    pub(crate) fn log_path() -> PathBuf {
        crate::job::cache_dir().join("linuxcnc-start.log")
    }

    fn openpty() -> std::io::Result<(OwnedFd, OwnedFd)> {
        let (mut m, mut s) = (0, 0);
        // SAFETY: openpty writes two fresh descriptors we then own.
        let r = unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both are open and owned by nobody else.
        Ok(unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) })
    }

    fn spawn(ini: &Path, restart: bool, hal_files: usize) -> Result<Run, String> {
        let sock = std::env::temp_dir().join(format!("millgui-boot-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).map_err(|e| format!("{}: {e}", sock.display()))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let (pty, tty) = openpty().map_err(|e| format!("terminal for LinuxCNC: {e}"))?;
        let mut cmd = Command::new("linuxcnc");
        cmd.arg("-v")
            .arg(ini)
            .env("MILLGUI_BOOT", &sock)
            .stdin(Stdio::from(tty))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        {
            use std::os::unix::process::CommandExt;
            // Its own group: a Ctrl-C here reaches millgui, which stops
            // LinuxCNC in order, not both at once.
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().map_err(|e| format!("could not run linuxcnc: {e} (is LinuxCNC installed and on PATH?)"))?;
        if restart {
            // "LinuxCNC is still running. Restart it? [Y/n]"
            let mut w = std::fs::File::from(pty.try_clone().map_err(|e| e.to_string())?);
            let _ = w.write_all(b"y\n");
        }
        let (tx, lines) = channel();
        for (err, r) in [(false, child.stdout.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>)), (true, child.stderr.take().map(|e| Box::new(e) as Box<dyn std::io::Read + Send>))] {
            let Some(r) = r else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || {
                for l in BufReader::new(r).lines().map_while(Result::ok) {
                    // After the splash the lines go to millgui's own output.
                    if tx.send((err, l.clone())).is_err() {
                        if err {
                            eprintln!("{l}");
                        } else {
                            println!("{l}");
                        }
                    }
                }
            });
        }
        let log = std::fs::create_dir_all(crate::job::cache_dir()).ok().and_then(|_| std::fs::File::create(log_path()).ok());
        Ok(Run {
            launcher: Launcher { child, _pty: pty, sock, attached: None, version: None },
            listener,
            lines,
            log,
            tail: VecDeque::new(),
            errors: Vec::new(),
            hal_files,
            hal_seen: 0,
            last_error: false,
            step: 0,
        })
    }

    /// Lines that say something went wrong. LinuxCNC's own stderr is mostly
    /// that; its stdout has a few known failure lines.
    fn is_error(err: bool, l: &str) -> bool {
        let t = l.trim();
        if noise(t) {
            return false;
        }
        err || ["Can't", "CANNOT", "Could not", "did not load", "failed", "ERROR", "Error", "error:"].iter().any(|k| t.contains(k))
    }

    /// The script's own wrap-up, not the reason.
    fn noise(t: &str) -> bool {
        t.is_empty() || t == "and" || NOISE.iter().any(|n| t.starts_with(n))
    }

    const NOISE: [&str; 8] = [
        "LinuxCNC terminated with an error",
        "as well as in the output",
        "/home/",
        "Shutting down and cleaning up",
        "Cleanup done",
        "Note: Using POSIX",
        "Verbose mode on",
        "Killing task",
    ];

    impl Run {
        /// Take the lines read so far and move the splash along.
        fn pump(&mut self, ui: &MainWindow) {
            while let Ok((err, l)) = self.lines.try_recv() {
                if let Some(f) = self.log.as_mut() {
                    let _ = writeln!(f, "{}{l}", if err { "! " } else { "" });
                }
                if err {
                    eprintln!("{l}");
                }
                // An indented line goes on from the error above it
                // ("twopass: Error in file ./mymill.hal:" then the reason).
                let goes_on = l.starts_with([' ', '\t']) && !noise(l.trim());
                if goes_on && self.last_error {
                    self.errors.push(l.trim().to_string());
                } else {
                    self.last_error = is_error(err, &l);
                    if self.last_error {
                        self.errors.push(l.trim().to_string());
                    }
                }
                self.tail.push_back(l.clone());
                if self.tail.len() > 12 {
                    self.tail.pop_front();
                }
                self.line(ui, l.trim());
            }
        }

        fn go(&mut self, ui: &MainWindow, n: usize, at: f32, doing: &str, detail: &str) {
            if n < self.step {
                return;
            }
            self.step = n;
            step(ui, n);
            let boot = ui.global::<Boot>();
            if at > boot.get_progress() {
                boot.set_progress(at);
            }
            say(ui, doing, detail);
        }

        fn line(&mut self, ui: &MainWindow, l: &str) {
            let boot = ui.global::<Boot>();
            if let Some(v) = l.strip_prefix("LINUXCNC - ") {
                self.launcher.version = Some(v.trim().to_string());
                let f = boot.get_footer().to_string();
                let ini = f.rsplit("  ·  ").next().unwrap_or(&f).to_string();
                boot.set_footer(format!("LinuxCNC {}  ·  {ini}", v.trim()).into());
            } else if l.starts_with("Starting LinuxCNC...") || l.starts_with("Cleaning up old LinuxCNC") {
                self.go(ui, 0, 0.03, "STARTING REALTIME", "");
            } else if let Some(p) = l.strip_prefix("Starting LinuxCNC server program: ") {
                self.go(ui, 0, 0.05, "STARTING REALTIME", &format!("Server: {p}"));
            } else if l.starts_with("Loading Real Time OS") {
                self.go(ui, 0, 0.08, "STARTING REALTIME", "Real-time OS, RTAPI and HAL");
            } else if let Some(p) = l.strip_prefix("Starting LinuxCNC IO program: ") {
                self.go(ui, 0, 0.11, "STARTING REALTIME", &format!("Tool and coolant I/O: {p}"));
            } else if let Some(p) = l.strip_prefix("Starting HAL User Interface program: ") {
                self.go(ui, 0, 0.13, "STARTING REALTIME", &format!("HAL user interface: {p}"));
            } else if let Some((_, f)) = l.strip_prefix("Found file(").and_then(|r| r.split_once("): ")) {
                self.hal_seen += 1;
                let n = self.hal_files.max(self.hal_seen);
                let name = Path::new(f).file_name().map_or(f.to_string(), |s| s.to_string_lossy().into_owned());
                let at = AT[1] + (AT[2] - AT[1]) * (self.hal_seen - 1) as f32 / n as f32;
                let detail = format!("{name}   ·   {} of {n}", self.hal_seen);
                self.go(ui, 1, at, "LOADING HAL", &detail);
            } else if let Some(rest) = l.strip_prefix("twopass:") {
                // [HAL]TWOPASS (mymill.ini): every file is read first, then
                // loaded in one go; no per-file lines. Each line it prints
                // takes a quarter of what is left of the HAL share.
                let p = boot.get_progress().max(AT[1]);
                let at = p + (AT[2] - 0.02 - p) * 0.25;
                if let Some(f) = rest.strip_prefix("found ") {
                    let name = Path::new(f.trim()).file_name().map_or(f.to_string(), |s| s.to_string_lossy().into_owned());
                    self.go(ui, 1, at, "LOADING HAL", &format!("Reading {name}"));
                } else if let Some(cmd) = rest.strip_prefix("pass0: loadusr ") {
                    // "-Wn sim_card ./sim_card.py": the program, without options.
                    let prog = cmd.split_whitespace().filter(|w| !w.starts_with('-')).last().unwrap_or(cmd);
                    let name = Path::new(prog).file_name().map_or(prog.to_string(), |s| s.to_string_lossy().into_owned());
                    self.go(ui, 1, at, "LOADING HAL", &format!("Starting {name}"));
                } else if rest.starts_with("invoked") {
                    self.go(ui, 1, AT[1], "LOADING HAL", "");
                }
            } else if l.starts_with("Starting TASK program: ") {
                self.go(ui, 2, AT[2], "STARTING MOTION", "Task and motion controller");
            } else if let Some(c) = l.strip_prefix("Running HAL command: ") {
                self.go(ui, 2, AT[2] + 0.06, "STARTING MOTION", c);
            } else if l.starts_with("Starting DISPLAY program: ") {
                self.go(ui, 3, AT[3], "OPENING THE SCREEN", "");
            }
        }

        fn failed(&mut self, ui: &MainWindow, status: std::process::ExitStatus) {
            // The last lines may still be in the pipes.
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_millis(300) {
                std::thread::sleep(Duration::from_millis(30));
                self.pump(ui);
            }
            fail_step(ui);
            let boot = ui.global::<Boot>();
            let mut why: Vec<String> = Vec::new();
            for e in &self.errors {
                if !why.contains(e) {
                    why.push(e.clone());
                }
            }
            let why: Vec<String> = if why.is_empty() {
                self.tail.iter().filter(|l| !noise(l.trim())).cloned().collect()
            } else {
                why
            };
            let keep = why.len().saturating_sub(8);
            let mut text = why[keep..].join("\n");
            if text.is_empty() {
                text = format!("LinuxCNC stopped ({status}) without saying why.");
            }
            say(ui, "LINUXCNC DID NOT START", "");
            boot.set_error(text.into());
            boot.set_log(log_path().display().to_string().into());
            boot.set_can_retry(true);
            boot.set_phase("failed".into());
        }
    }

    #[cfg(test)]
    #[test]
    fn errors_are_picked_out() {
        assert!(is_error(true, "./mymill.hal:12: Pin 'x.y' does not exist"));
        assert!(is_error(false, "Can't execute TASK program milltask"));
        assert!(!is_error(false, "Found file(REL): ./mymill.hal"));
        assert!(!is_error(true, "and"));
        assert!(!is_error(true, "Shutting down and cleaning up LinuxCNC..."));
        assert!(is_error(false, "twopass:CANNOT FIND FILE FOR:x.hal"));
    }

    enum Next {
        Attached(UnixStream),
        Exited(std::process::ExitStatus),
    }

    /// TRY AGAIN or RESTART pressed (true), or the user left (false).
    async fn until_retry(ui: &MainWindow, retry: &Rc<std::cell::Cell<bool>>) -> bool {
        retry.set(false);
        let r = retry.clone();
        matches!(wait_until(ui, move |_| r.get().then_some(())).await, Wait::Done(()))
    }

    /// Start LinuxCNC with the splash up. Returns once its display step has
    /// started (millgui then connects as usual), or None to exit.
    pub async fn launch(ui: &MainWindow, ini: &Path, cfg: &Config) -> Option<Launcher> {
        let boot = ui.global::<Boot>();
        let ini = std::fs::canonicalize(ini).unwrap_or_else(|_| ini.to_path_buf());
        // Relative paths in the INI are from its folder, as for LinuxCNC's display.
        if let Some(d) = ini.parent() {
            let _ = std::env::set_current_dir(d);
        }
        // Another LinuxCNC screen would never hand over: say so up front.
        let display = cfg.get("DISPLAY", "DISPLAY").unwrap_or("");
        let prog = display.split_whitespace().next().unwrap_or("");
        let prog = Path::new(prog).file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned());
        const OTHERS: [&str; 11] = ["axis", "gmoccapy", "touchy", "gscreen", "tklinuxcnc", "qtvcp", "probe_basic", "qtdragon", "qtplasmac", "dummy", "linuxcncrsh"];
        if OTHERS.contains(&prog.as_str()) {
            fail_step(ui);
            say(ui, "THIS INI DOES NOT USE MILLGUI", "");
            boot.set_error(format!("{} has DISPLAY = {display}. Boot mode starts LinuxCNC with millgui as its screen; use millgui.ini, whose DISPLAY is ./millgui/run.", ini.display()).into());
            boot.set_phase("failed".into());
            until_quit().await;
            return None;
        }
        let hal_files = cfg.get_all("HAL", "HALFILE").len();
        let retry = Rc::new(std::cell::Cell::new(false));
        {
            let retry = retry.clone();
            boot.on_retry(move || retry.set(true));
        }
        // A retry after a failure, or "restart" when another session holds the lock.
        let mut restart = false;
        loop {
            if !restart && Path::new(LOCK).exists() {
                say(ui, "LINUXCNC IS ALREADY RUNNING", "");
                boot.set_error("Another LinuxCNC session is running, or the last one did not shut down cleanly. Restarting stops that session first.".into());
                boot.set_log("".into());
                boot.set_can_retry(true);
                boot.set_phase("lock".into());
                if !until_retry(ui, &retry).await {
                    return None;
                }
                restart = true;
            }
            show(ui, 0, footer(None, Some(&ini)));
            let run = match spawn(&ini, restart, hal_files) {
                Ok(r) => r,
                Err(e) => {
                    fail_step(ui);
                    say(ui, "LINUXCNC DID NOT START", "");
                    boot.set_error(e.into());
                    boot.set_can_retry(true);
                    boot.set_phase("failed".into());
                    if until_retry(ui, &retry).await {
                        restart = false;
                        continue;
                    }
                    return None;
                }
            };
            let run = Rc::new(RefCell::new(Some(run)));
            let r2 = run.clone();
            let got = wait_until(ui, move |ui| {
                let mut g = r2.borrow_mut();
                let r = g.as_mut()?;
                r.pump(ui);
                if let Ok((s, _)) = r.listener.accept() {
                    return Some(Next::Attached(s));
                }
                match r.launcher.child.try_wait() {
                    Ok(Some(st)) => Some(Next::Exited(st)),
                    _ => None,
                }
            })
            .await;
            let mut r = run.borrow_mut().take().unwrap();
            match got {
                Wait::Done(Next::Attached(s)) => {
                    r.pump(ui);
                    let _ = s.set_nonblocking(false);
                    r.launcher.attached = Some(s);
                    step(ui, 3);
                    say(ui, "CONNECTING", "LinuxCNC is up; millgui is connecting to it");
                    boot.set_progress(AT[3] + 0.06);
                    // From here LinuxCNC's output goes to millgui's own.
                    drop(r.lines);
                    return Some(r.launcher);
                }
                Wait::Done(Next::Exited(st)) => {
                    r.failed(ui, st);
                    if until_retry(ui, &retry).await {
                        restart = false;
                        continue;
                    }
                    return None;
                }
                Wait::Quit => {
                    // EXIT while starting: stop what has started.
                    r.launcher.shutdown(ui).await;
                    return None;
                }
            }
        }
    }

    impl Launcher {
        /// LinuxCNC stopped on its own while millgui runs.
        pub fn exited(&mut self) -> bool {
            matches!(self.child.try_wait(), Ok(Some(_)))
        }

        /// Stop LinuxCNC (close the display's connection, or signal the
        /// script before it got that far) and wait for it, splash up. A
        /// further EXIT or signal does not cut this short.
        pub async fn shutdown(mut self, ui: &MainWindow) {
            let boot = ui.global::<Boot>();
            say(ui, "STOPPING LINUXCNC", "");
            boot.set_phase("stopping".into());
            super::STOPPING.store(true, std::sync::atomic::Ordering::SeqCst);
            if self.attached.take().is_none() {
                // SAFETY: plain kill(2) on our own child's pid.
                unsafe {
                    libc::kill(self.child.id() as i32, libc::SIGTERM);
                }
            }
            let t0 = Instant::now();
            every(Duration::from_millis(50), move || {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return Some(());
                }
                if t0.elapsed() > Duration::from_secs(20) {
                    eprintln!("millgui: LinuxCNC did not stop within 20 s; killing it");
                    // SAFETY: as above, the whole group we started.
                    unsafe {
                        libc::kill(-(self.child.id() as i32), libc::SIGKILL);
                    }
                    let _ = self.child.wait();
                    return Some(());
                }
                None
            })
            .await;
        }
    }

    impl Drop for Launcher {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.sock);
        }
    }
}

#[cfg(unix)]
pub use launch::launch;

#[cfg(test)]
mod tests {
    #[test]
    fn footer_reads_well() {
        assert_eq!(super::footer(Some("2.9.10"), Some(std::path::Path::new("/x/millgui.ini"))), "LinuxCNC 2.9.10  ·  millgui.ini");
        assert_eq!(super::footer(None, None), "");
    }
}
