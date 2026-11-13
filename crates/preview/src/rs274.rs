//! Run LinuxCNC's stand-alone interpreter on a program.
//!
//! Safety: LinuxCNC 2.9's `rs274` truncates `$HOME/.tool.mmap` before it
//! parses its arguments, which would clear the running machine's tool table
//! (see the repo's AGENTS.md). It therefore always runs with a private HOME.
//! It also writes its parameter file on exit, so it gets a copy.

use crate::Toolpath;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Setup {
    /// INI for the preview: the machine INI without REMAP lines (remaps read
    /// HAL pins, which do not exist outside LinuxCNC; the built-in M6 is
    /// enough for a preview).
    pub ini: PathBuf,
    /// Working directory, so relative SUBROUTINE_PATH entries resolve.
    pub config_dir: PathBuf,
    pub var_file: Option<PathBuf>,
    pub tool_table: Option<PathBuf>,
    pub rapid_mm_s: f64,
}

/// Whether `rs274` is on PATH (checked without running it).
pub fn available() -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("rs274").is_file()))
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("millgui-rs274-{}-{}", std::process::id(), unique()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

pub fn run(setup: &Setup, program: &Path) -> Toolpath {
    let dir = scratch();
    let home = dir.join("home");
    let _ = std::fs::create_dir_all(&home);
    let mut cmd = Command::new("rs274");
    cmd.current_dir(&setup.config_dir).env("HOME", &home).arg("-i").arg(&setup.ini);
    if let Some(v) = &setup.var_file {
        let copy = dir.join("params.var");
        if std::fs::copy(v, &copy).is_ok() {
            cmd.arg("-v").arg(copy);
        }
    }
    if let Some(t) = &setup.tool_table {
        let copy = dir.join("tool.tbl");
        if std::fs::copy(t, &copy).is_ok() {
            cmd.arg("-t").arg(copy);
        }
    }
    cmd.arg("-g").arg(program);
    let out = cmd.output();
    let _ = std::fs::remove_dir_all(&dir);
    match out {
        Ok(o) => {
            let mut tp = crate::canon::parse(&String::from_utf8_lossy(&o.stdout), setup.rapid_mm_s);
            let err = String::from_utf8_lossy(&o.stderr);
            // rs274 prints "executing" and, on failure, the error and the line.
            let msg: Vec<&str> = err.lines().map(str::trim).filter(|l| !l.is_empty() && *l != "executing").collect();
            if !msg.is_empty() {
                tp.error = Some(msg.join(" · "));
            }
            tp
        }
        Err(e) => Toolpath { error: Some(format!("rs274: {e}")), ..Default::default() },
    }
}
