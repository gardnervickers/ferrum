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
    // A copy with each line numbered (N<line>), so the canonical calls say
    // which source line they come from; the program itself is untouched.
    let numbered = dir.join("program.ngc");
    let source = std::fs::read(program).map(|b| String::from_utf8_lossy(&b).into_owned());
    let machine_lines = source.as_ref().map(|t| crate::machine_lines(t)).unwrap_or_default();
    let find_lines = source.as_ref().map(|t| crate::find_lines(t)).unwrap_or_default();
    let program = match &source {
        Ok(text) if std::fs::write(&numbered, number_lines(text)).is_ok() => numbered.as_path(),
        _ => program,
    };
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
            tp.machine_lines = machine_lines;
            tp.find_lines = find_lines;
            tp
        }
        Err(e) => Toolpath { error: Some(format!("rs274: {e}")), ..Default::default() },
    }
}

/// Put `N<line number>` at the start of every block, replacing an N word
/// already there. A block-delete slash stays first; blank lines, `%` and
/// lines past N's range are left alone.
pub fn number_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let n = i + 1;
        let body = line.trim_start();
        if body.trim().is_empty() || body.starts_with('%') || n > 99_999_999 {
            out.push_str(line);
            continue;
        }
        let (slash, rest) = match body.strip_prefix('/') {
            Some(r) => ("/", r.trim_start()),
            None => ("", body),
        };
        let rest = match rest.strip_prefix(['N', 'n']) {
            Some(r) if r.starts_with(|c: char| c.is_ascii_digit()) => r.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.'),
            _ => rest,
        };
        out.push_str(slash);
        out.push_str(&format!("N{n} "));
        out.push_str(rest);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::number_lines;

    #[test]
    fn g53_lines_are_found_outside_comments() {
        let src = "G21\nG53 G0 Z0\n  g53g0z0 (up)\n(G53 in a comment)\nG0 X1 ; G53\nG54\n";
        assert_eq!(crate::machine_lines(src), [2, 3]);
    }

    #[test]
    fn numbers_blocks_and_keeps_the_rest() {
        let src = "%\nG21\n\n/G0 X1\nN10 G1 X2\n  (note)\no100 if [1]\n%\n";
        assert_eq!(number_lines(src), "%\nN2 G21\n\n/N4 G0 X1\nN5  G1 X2\nN6 (note)\nN7 o100 if [1]\n%\n");
    }
}
