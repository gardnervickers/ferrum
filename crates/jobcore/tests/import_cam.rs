//! Adding a Fusion program to a job: all ops or none, with the reason.

use jobcore::Helper;
use serde_json::{json, Value};
use std::path::PathBuf;

fn setup(name: &str) -> (Helper, PathBuf) {
    let dir = std::env::temp_dir().join(format!("jobcore-import-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("jobs")).unwrap();
    std::fs::write(dir.join("tool.tbl"), "T14 P14 Z+100 D+50 ;face\nT23 P23 Z+90 D+6.35 ;end mill\nT99 P99 Z+120 D+3 ;probe\n").unwrap();
    let mut h = Helper::new(dir.clone());
    let r = h.handle(&json!({"cmd": "init", "folder": dir.join("jobs"), "tool_table": dir.join("tool.tbl"),
                             "nc_folder": "nc", "probe_tool": 99, "max_forward_rpm": 10000}));
    assert!(r.get("error").is_none(), "{r}");
    h.handle(&json!({"cmd": "new"}));
    (h, dir)
}

fn ops(r: &Value) -> Vec<String> {
    r["snap"]["ops"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn adds_every_op_of_a_fusion_program() {
    let (mut h, _dir) = setup("ok");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/nc_files/bracket-sample.ngc");
    let r = h.handle(&json!({"cmd": "import_cam", "path": path}));
    assert_eq!(r["message"], "ADDED 2 OPS FROM BRACKET-SAMPLE.NGC.", "{r}");
    assert_eq!(ops(&r), ["FACE1", "2D CONTOUR1"]);
    let r = h.handle(&json!({"cmd": "build"}));
    assert!(r.get("error").is_none(), "{r}");
}

#[test]
fn refuses_the_whole_program_with_the_reason() {
    let (mut h, dir) = setup("bad");
    // The second op is fine for the splitter but over the spindle maximum.
    let p = dir.join("fast.ngc");
    std::fs::write(&p, "G90 G21\n(A)\nT23 M6\nS8000 M3\nG54\nG0 X0 Y0\nG43 Z5 H23\nG1 Z-1 F100\nG0 Z5\n(B)\nS12000 M3\nG0 X5 Y5\nG0 Z2\nG1 Z-1\n").unwrap();
    let r = h.handle(&json!({"cmd": "import_cam", "path": p}));
    let text = r.to_string();
    assert!(text.contains("FAST.NGC was not added") && text.contains("spindle maximum") && text.contains("Run it whole"), "{text}");
    let r = h.handle(&json!({"cmd": "poll"}));
    assert!(r["snap"]["ops"].as_array().map_or(true, |a| a.is_empty()), "nothing added: {r}");
    let p = dir.join("probe.ngc");
    std::fs::write(&p, "G90 G21\n(PROBE1)\nT99 M6\no<f360_probe_z> call [1]\n").unwrap();
    let text = h.handle(&json!({"cmd": "import_cam", "path": p})).to_string();
    assert!(text.contains("o-words or parameters"), "{text}");
}
