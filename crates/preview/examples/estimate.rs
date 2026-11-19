//! Print the run-time estimate of programs, read through rs274 with an INI:
//!   cargo run -p preview --example estimate -- machine.ini a.ngc b.ngc
//! (rs274 on PATH: source LinuxCNC's rip-environment first.)
use preview::{rs274, timing};
use std::path::PathBuf;

fn ini_get(text: &str, section: &str, key: &str) -> Option<f64> {
    let mut sec = String::new();
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            sec = l.trim_matches(|c| c == '[' || c == ']').to_string();
        } else if sec == section {
            if let Some((k, v)) = l.split_once('=') {
                if k.trim() == key {
                    return v.trim().parse().ok();
                }
            }
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ini = PathBuf::from(&args[0]);
    let text = std::fs::read_to_string(&ini).unwrap();
    let mut lim = timing::Limits::default();
    for (i, a) in ["X", "Y", "Z", "A"].iter().enumerate() {
        lim.vel[i] = ini_get(&text, &format!("AXIS_{a}"), "MAX_VELOCITY").unwrap_or(0.0);
        lim.acc[i] = ini_get(&text, &format!("AXIS_{a}"), "MAX_ACCELERATION").unwrap_or(0.0);
    }
    lim.max_linear_vel = ini_get(&text, "TRAJ", "MAX_LINEAR_VELOCITY").unwrap_or(0.0);
    lim.max_linear_acc = ini_get(&text, "TRAJ", "MAX_LINEAR_ACCELERATION").unwrap_or(0.0);
    lim.io_wait = ini_get(&text, "EMCIO", "CYCLE_TIME").map_or(lim.io_wait, |c| c * 1.2);
    lim.servo_period = ini_get(&text, "EMCMOT", "SERVO_PERIOD").unwrap_or(1e6) * 1e-9;
    let setup = rs274::Setup {
        ini: ini.clone(),
        config_dir: ini.parent().unwrap().to_path_buf(),
        var_file: None,
        tool_table: None,
        rapid_mm_s: lim.max_linear_vel,
        inch: false,
    };
    for p in &args[1..] {
        let mut tp = rs274::run(&setup, &PathBuf::from(p));
        let naive = tp.seconds();
        timing::estimate(&mut tp, &lim);
        println!("{p}\tnaive {naive:.2}\testimate {:.2}\tmoves {}", tp.seconds(), tp.moves.len());
    }
}
