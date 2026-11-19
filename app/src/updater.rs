//! SETUP › SOFTWARE: Ferrum's version and its updates, through the
//! installer's `ferrum-update` tool (scripts/ferrum-update). Ferrum never
//! swaps itself: it asks the tool to download and check a version, and the
//! launcher puts it into use at the next start. Every command here is safe
//! while LinuxCNC runs.
//!
//! The tool is `PREFIX/ferrum-update`, where the running binary is
//! `PREFIX/versions/<v>/bin/ferrum` and `PREFIX/current` is a link;
//! `$FERRUM_UPDATER` names another (tests, the simulator's screenshots).

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

/// `ferrum-update status --json`. Every key is always there.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Status {
    #[serde(deserialize_with = "or_empty")]
    pub installed_in: String,
    /// Null from the tool when nothing is installed yet; empty here.
    #[serde(deserialize_with = "or_empty")]
    pub current: String,
    pub previous: Option<String>,
    pub installed: Vec<String>,
    #[serde(deserialize_with = "or_empty")]
    pub channel: String,
    pub auto: bool,
    pub available: Option<String>,
    pub staged: Option<String>,
    pub on_trial: Option<String>,
    pub fell_back: Option<String>,
    pub skipped: Option<String>,
    pub last_check: Option<String>,
    pub last_error: Option<String>,
}

fn or_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

impl Status {
    /// A newer version than the one in use is known.
    pub fn newer(&self) -> Option<&str> {
        self.available.as_deref().filter(|v| *v != self.current && newer_than(v, &self.current))
    }
}

/// `a` is a later version than `b` (dotted numbers; anything else by text).
pub fn newer_than(a: &str, b: &str) -> bool {
    let parts = |s: &str| -> Option<Vec<u64>> { s.trim_start_matches('v').split(['.', '-', '+']).take(3).map(|p| p.parse().ok()).collect() };
    match (parts(a), parts(b)) {
        (Some(x), Some(y)) => x > y,
        _ => a != b,
    }
}

/// The updater tool, if this Ferrum was installed with it.
pub fn tool() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FERRUM_UPDATER") {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    tool_for(&exe)
}

fn tool_for(exe: &Path) -> Option<PathBuf> {
    // PREFIX/versions/<v>/bin/ferrum
    let prefix = exe.parent()?.parent()?.parent()?.parent()?;
    let linked = std::fs::symlink_metadata(prefix.join("current")).is_ok_and(|m| m.file_type().is_symlink());
    let t = prefix.join("ferrum-update");
    (linked && t.is_file()).then_some(t)
}

/// What a command was for, so its answer lands in the right place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Status,
    Check,
    Stage,
    Unstage,
    Rollback,
    Channel,
    Auto,
}

impl Op {
    /// Said while it runs.
    pub fn doing(self) -> &'static str {
        match self {
            Op::Status => "Reading the updater",
            Op::Check => "Checking for updates",
            Op::Stage => "Downloading and checking",
            Op::Unstage => "Cancelling",
            Op::Rollback => "Setting up the roll back",
            Op::Channel | Op::Auto => "Saving",
        }
    }
}

pub struct Reply {
    pub op: Op,
    /// The command's error line, if it failed.
    pub error: Option<String>,
    /// The status read after it.
    pub status: Option<Status>,
}

/// Runs the tool on a thread; `poll` hands back answers on the UI thread.
pub struct Updater {
    tool: Option<PathBuf>,
    tx: mpsc::Sender<Reply>,
    rx: mpsc::Receiver<Reply>,
    pub busy: Option<Op>,
}

fn run(tool: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(tool).args(args).output().map_err(|e| format!("{}: {e}", tool.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let e = String::from_utf8_lossy(&out.stderr);
        Err(e.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("the updater failed").trim().to_string())
    }
}

pub fn read_status(tool: &Path) -> Result<Status, String> {
    let out = run(tool, &["status", "--json"])?;
    serde_json::from_str(&out).map_err(|e| format!("the updater's status does not read: {e}"))
}

impl Updater {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Updater { tool: tool(), tx, rx, busy: None }
    }

    pub fn present(&self) -> bool {
        self.tool.is_some()
    }

    /// Start `op` (with its arguments) unless one is running.
    pub fn start(&mut self, op: Op, args: Vec<String>) {
        let Some(tool) = self.tool.clone() else { return };
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(op);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let error = if op == Op::Status {
                None
            } else {
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                run(&tool, &a).err()
            };
            let (status, error) = match read_status(&tool) {
                Ok(s) => (Some(s), error),
                Err(e) => (None, error.or(Some(e))),
            };
            let _ = tx.send(Reply { op, error, status });
        });
    }

    /// Read the status now, on this thread (screenshots).
    pub fn status_now(&self) -> Option<Result<Status, String>> {
        self.tool.as_deref().map(read_status)
    }

    pub fn poll(&mut self) -> Option<Reply> {
        let r = self.rx.try_recv().ok()?;
        self.busy = None;
        Some(r)
    }
}

/// The command a SOFTWARE action runs.
pub fn command(action: &str, s: Option<&Status>) -> Option<(Op, Vec<String>)> {
    let v = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    Some(match action {
        "upd-check" => (Op::Check, v(&["check"])),
        "upd-stage" => match s.and_then(|s| s.newer()) {
            Some(ver) => (Op::Stage, v(&["stage", "--version", ver])),
            None => (Op::Stage, v(&["stage"])),
        },
        "upd-unstage" => (Op::Unstage, v(&["unstage"])),
        "upd-rollback" => (Op::Rollback, v(&["rollback", "--on-restart"])),
        "upd-channel" => {
            let next = if s.is_some_and(|s| s.channel == "edge") { "stable" } else { "edge" };
            (Op::Channel, v(&["channel", next]))
        }
        "upd-auto" => (Op::Auto, v(&["auto", if s.is_some_and(|s| s.auto) { "off" } else { "on" }])),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer_than("0.10.0", "0.9.3"));
        assert!(newer_than("v1.2.0", "1.1.9"));
        assert!(!newer_than("0.4.0", "0.4.0"));
        assert!(!newer_than("0.3.9", "0.4.0"));
        assert!(newer_than("edge-abc", "0.4.0"));
    }

    #[test]
    fn status_reads_the_tools_json() {
        let j = r#"{"installed_in":"/home/cnc/ferrum","current":"0.4.0","previous":"0.3.2","installed":["0.4.0","0.3.2"],
            "channel":"stable","auto":false,"available":"0.4.1","staged":null,"on_trial":null,"fell_back":null,
            "skipped":null,"last_check":"2026-11-19T14:00:00Z","last_error":null,"running":true}"#;
        let s: Status = serde_json::from_str(j).unwrap();
        assert_eq!(s.newer(), Some("0.4.1"));
        assert_eq!(command("upd-stage", Some(&s)).unwrap().1, ["stage", "--version", "0.4.1"]);
        assert_eq!(command("upd-channel", Some(&s)).unwrap().1, ["channel", "edge"]);
        assert_eq!(command("upd-auto", Some(&s)).unwrap().1, ["auto", "on"]);
        assert_eq!(command("upd-rollback", Some(&s)).unwrap().1, ["rollback", "--on-restart"]);
    }

    #[test]
    fn the_tool_sits_three_folders_above_the_binary() {
        let d = std::env::temp_dir().join(format!("ferrum-upd-{}", std::process::id()));
        let bin = d.join("versions/0.4.0/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("ferrum"), "").unwrap();
        assert_eq!(tool_for(&bin.join("ferrum")), None);
        std::fs::write(d.join("ferrum-update"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.join("versions/0.4.0"), d.join("current")).unwrap();
        #[cfg(unix)]
        assert_eq!(tool_for(&bin.join("ferrum")), Some(d.join("ferrum-update")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
