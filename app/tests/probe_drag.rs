//! Dragging a 3-point bore touch around the circle, natively and as the
//! browser screen sees it (every Live property re-set after each event).
//! The browser screen used to replace list models on every update, which
//! rebuilt the points under the finger and dropped the drag.

use std::process::Command;

/// Drag touch 1 from the top of the circle (90°) round to 20°, and return
/// what `--show drag*:` printed for the points.
fn drag(kind: &str) -> String {
    // The circle on the PROBE picture at 1920x1080: centre and radius in px.
    let (cx, cy, r) = (1155.0_f64, 477.0_f64, 90.0_f64);
    let path: Vec<String> = (0..=14)
        .map(|i| {
            let a = (90.0 - 5.0 * i as f64).to_radians();
            format!("{:.0},{:.0}", cx + r * a.cos(), cy - r * a.sin())
        })
        .collect();
    let out = std::env::temp_dir().join(format!("ferrum-drag-{kind}-{}.png", std::process::id()));
    let o = Command::new(env!("CARGO_BIN_EXE_ferrum"))
        .args(["--sim", "--screen", "probe", "--show", "probe-shape:bore", "--show", "probe-opt:pts-3", "--show"])
        .arg(format!("{kind}:{}", path.join(";")))
        .arg("--screenshot")
        .arg(&out)
        .output()
        .expect("run ferrum");
    let _ = std::fs::remove_file(&out);
    let err = String::from_utf8_lossy(&o.stderr);
    err.lines().find_map(|l| l.strip_prefix("points: ")).unwrap_or_else(|| panic!("no points line:\n{err}")).to_string()
}

#[test]
fn bore_point_follows_the_drag_natively() {
    let p = drag("drag");
    assert!(!p.starts_with("1@200,90 "), "touch 1 did not move: {p}");
}

#[test]
fn bore_point_follows_the_drag_in_the_browser() {
    let p = drag("drag-web");
    assert!(!p.starts_with("1@200,90 "), "touch 1 did not move: {p}");
    assert_eq!(p, drag("drag"), "browser drag ended somewhere else");
}
