//! millgui-harness: end-to-end scenarios against the LinuxCNC simulator.
//! See ../README.md. Normally started through `millgui/harness/run`.

mod eval;
mod image;
mod script;
mod session;

use script::{Scenario, Step};
use serde_json::{json, Value};
use session::{Env, Session};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

struct Args {
    configs: Option<Vec<String>>,
    scenarios: Vec<String>,
    out: PathBuf,
    lcnc: PathBuf,
    millgui: PathBuf,
    display: Option<String>,
    update_goldens: bool,
    list: bool,
    port: u16,
}

fn usage() -> ! {
    eprintln!(
        "usage: millgui-harness [--configs a,b] [--out DIR] [--lcnc DIR] [--millgui BIN]\n\
         \x20                      [--display :N] [--port N] [--update-goldens] [--list] [SCENARIO...]\n\
         SCENARIO is a name under scenarios/ (\"basics/home\"), a directory of them, or a path."
    );
    std::process::exit(2)
}

fn harness_dir() -> PathBuf {
    std::env::var_os("HARNESS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn parse_args() -> Args {
    let home = std::env::var("HOME").unwrap_or_default();
    let hd = harness_dir();
    let mut a = Args {
        configs: None,
        scenarios: vec![],
        out: PathBuf::from("harness-out"),
        // The LinuxCNC run-in-place tree (with scripts/rip-environment).
        lcnc: std::env::var_os("LCNC_DIR")
            .map(|d| PathBuf::from(d).join("src"))
            .unwrap_or_else(|| Path::new(&home).join("lcnc/src")),
        millgui: std::env::var_os("MILLGUI_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| hd.join("../target-nml/debug/millgui")),
        display: None,
        update_goldens: false,
        list: false,
        port: 8099,
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--configs" | "-c" => {
                a.configs = it.next().map(|v| v.split(',').map(String::from).collect())
            }
            "--out" | "-o" => a.out = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--lcnc" => a.lcnc = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--millgui" => a.millgui = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--display" => a.display = it.next(),
            "--port" => {
                a.port = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            "--update-goldens" => a.update_goldens = true,
            "--list" => a.list = true,
            "-h" | "--help" => usage(),
            s if s.starts_with('-') => usage(),
            s => a.scenarios.push(s.to_string()),
        }
    }
    a
}

struct Config {
    name: String,
    dir: PathBuf,
    vars: HashMap<String, String>,
    tags: Vec<String>,
}

fn load_configs(root: &Path) -> Vec<Config> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.join("prepare").exists() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let mut vars = HashMap::from([("config".to_string(), name.clone())]);
        // `vars`, then `vars.extra` on top (a config that shares another's
        // vars through a link overrides a few there).
        for t in ["vars", "vars.extra"].iter().filter_map(|f| std::fs::read_to_string(dir.join(f)).ok()) {
            for l in t.lines() {
                let l = l.split(" #").next().unwrap_or("").trim();
                if l.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = l.split_once('=') {
                    vars.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
        let mut tags: Vec<String> = vars
            .get("tags")
            .map(|t| t.split_whitespace().map(String::from).collect())
            .unwrap_or_default();
        tags.push(name.clone());
        out.push(Config {
            name,
            dir,
            vars,
            tags,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn find_scenarios(root: &Path, wanted: &[String]) -> Result<Vec<PathBuf>, String> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        v.sort();
        for p in v {
            if p.is_dir() {
                // common/ holds included pieces, not scenarios.
                if p.file_name().is_some_and(|n| n != "common") {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|e| e == "scn") {
                out.push(p);
            }
        }
    }
    let mut out = vec![];
    if wanted.is_empty() {
        walk(root, &mut out);
        return Ok(out);
    }
    for w in wanted {
        let cands = [
            PathBuf::from(w),
            root.join(w),
            root.join(format!("{w}.scn")),
        ];
        match cands.iter().find(|p| p.exists()) {
            Some(p) if p.is_dir() => walk(p, &mut out),
            Some(p) => out.push(p.clone()),
            None => return Err(format!("no scenario '{w}' under {}", root.display())),
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Outcome {
    Pass,
    Fail,
    XFail,
    XPass,
    Skip,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::XFail => "XFAIL",
            Outcome::XPass => "XPASS",
            Outcome::Skip => "SKIP",
        }
    }
}

struct Result_ {
    config: String,
    scenario: String,
    title: String,
    outcome: Outcome,
    detail: String,
    secs: f64,
    shots: Vec<(String, PathBuf)>,
    steps: Vec<String>,
    log: Option<PathBuf>,
}

fn start_display(n: &str) -> Result<Vec<std::process::Child>, String> {
    let mut kids = vec![];
    let x = Command::new("Xvfb")
        .args([n, "-screen", "0", "1920x1080x24", "-ac", "-nolisten", "tcp"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Xvfb: {e}"))?;
    kids.push(x);
    let t0 = Instant::now();
    let sock = format!("/tmp/.X11-unix/X{}", n.trim_start_matches(':'));
    while !Path::new(&sock).exists() {
        if t0.elapsed() > Duration::from_secs(10) {
            return Err(format!("Xvfb {n} did not start"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // A window manager gives millgui a full-size window.
    if let Ok(wm) = Command::new("openbox")
        .env("DISPLAY", n)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        kids.push(wm);
    }
    Ok(kids)
}

fn main() -> ExitCode {
    let a = parse_args();
    let hd = harness_dir();
    let configs = load_configs(&hd.join("configs"));
    let sc_root = hd.join("scenarios");
    let files = match find_scenarios(&sc_root, &a.scenarios) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let configs: Vec<&Config> = match &a.configs {
        Some(want) => {
            let mut v = vec![];
            for w in want {
                match configs.iter().find(|c| &c.name == w) {
                    Some(c) => v.push(c),
                    None => {
                        eprintln!(
                            "no config '{w}' (have: {})",
                            configs
                                .iter()
                                .map(|c| c.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                        return ExitCode::from(2);
                    }
                }
            }
            v
        }
        None => configs.iter().collect(),
    };
    if a.list {
        for c in &configs {
            println!("config {}  tags: {}", c.name, c.tags.join(" "));
        }
        for f in &files {
            println!(
                "scenario {}",
                f.strip_prefix(&sc_root).unwrap_or(f).display()
            );
        }
        return ExitCode::SUCCESS;
    }
    if !a.millgui.exists() {
        eprintln!(
            "no millgui build at {} (build with the NML backend; see README.md)",
            a.millgui.display()
        );
        return ExitCode::from(2);
    }
    let _ = std::fs::remove_dir_all(&a.out);
    std::fs::create_dir_all(&a.out).ok();
    let out = a.out.canonicalize().unwrap_or(a.out.clone());

    let (display, mut xkids) = match &a.display {
        Some(d) => (d.clone(), vec![]),
        None => {
            let d = ":97".to_string();
            match start_display(&d) {
                Ok(k) => (d, k),
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::from(2);
                }
            }
        }
    };
    let env = Env {
        lcnc: a.lcnc.clone(),
        millgui_bin: a.millgui.canonicalize().unwrap_or(a.millgui.clone()),
        display,
        port: a.port,
        harness_dir: hd.clone(),
    };

    let mut results = vec![];
    for c in &configs {
        for f in &files {
            let sc = match Scenario::load(f, &sc_root, &c.vars) {
                Ok(s) => s,
                Err(e) => {
                    let name = f
                        .strip_prefix(&sc_root)
                        .unwrap_or(f)
                        .with_extension("")
                        .display()
                        .to_string();
                    println!("[{}] {name}: FAIL (scenario: {e})", c.name);
                    results.push(Result_ {
                        config: c.name.clone(),
                        scenario: name.clone(),
                        title: name,
                        outcome: Outcome::Fail,
                        detail: e,
                        secs: 0.0,
                        shots: vec![],
                        steps: vec![],
                        log: None,
                    });
                    continue;
                }
            };
            let missing: Vec<_> = sc
                .needs
                .iter()
                .filter(|t| !c.tags.contains(t))
                .cloned()
                .collect();
            if !missing.is_empty() {
                results.push(Result_ {
                    config: c.name.clone(),
                    scenario: sc.name.clone(),
                    title: sc.title.clone(),
                    outcome: Outcome::Skip,
                    detail: format!("config lacks {}", missing.join(", ")),
                    secs: 0.0,
                    shots: vec![],
                    steps: vec![],
                    log: None,
                });
                println!(
                    "[{}] {}: SKIP (needs {})",
                    c.name,
                    sc.name,
                    missing.join(", ")
                );
                continue;
            }
            if let Some(e) = &sc.error {
                println!("[{}] {}: FAIL (scenario: {e})", c.name, sc.name);
                results.push(Result_ {
                    config: c.name.clone(),
                    scenario: sc.name.clone(),
                    title: sc.title.clone(),
                    outcome: Outcome::Fail,
                    detail: e.clone(),
                    secs: 0.0,
                    shots: vec![],
                    steps: vec![],
                    log: None,
                });
                continue;
            }
            let r = run_one(&env, c, &sc, &out, &hd, a.update_goldens);
            println!(
                "[{}] {}: {} ({:.1}s){}",
                c.name,
                sc.name,
                r.outcome.label(),
                r.secs,
                if r.detail.is_empty() {
                    String::new()
                } else {
                    format!("\n    {}", r.detail.replace('\n', "\n    "))
                }
            );
            results.push(r);
        }
    }
    report(&out, &results);
    for k in xkids.iter_mut().rev() {
        let _ = k.kill();
        let _ = k.wait();
    }
    let fails = results
        .iter()
        .filter(|r| r.outcome == Outcome::Fail)
        .count();
    let count = |o| results.iter().filter(|r| r.outcome == o).count();
    println!(
        "\n{} passed, {} failed, {} expected failures, {} unexpected passes, {} skipped. Report: {}",
        count(Outcome::Pass),
        fails,
        count(Outcome::XFail),
        count(Outcome::XPass),
        count(Outcome::Skip),
        out.join("index.html").display()
    );
    if fails > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn prepare(c: &Config, dir: &Path, port: u16) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let o = Command::new(c.dir.join("prepare"))
        .arg(dir)
        .output()
        .map_err(|e| format!("prepare: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "prepare failed: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ));
    }
    // The harness's own programs, with the config's ${vars} filled in.
    if let Ok(rd) = std::fs::read_dir(harness_dir().join("nc_files")) {
        std::fs::create_dir_all(dir.join("nc_files")).map_err(|e| e.to_string())?;
        for e in rd.flatten() {
            let text = std::fs::read_to_string(e.path()).map_err(|e| e.to_string())?;
            let mut t = text.clone();
            for (k, v) in &c.vars {
                t = t.replace(&format!("${{{k}}}"), v);
            }
            if let Some(i) = t.find("${") {
                return Err(format!(
                    "{}: config {} has no var for {}",
                    e.path().display(),
                    c.name,
                    &t[i..t[i..].find('}').map_or(t.len(), |j| i + j + 1)]
                ));
            }
            std::fs::write(dir.join("nc_files").join(e.file_name()), t)
                .map_err(|e| e.to_string())?;
        }
    }
    // millgui's websocket on the harness port.
    let ini = dir.join("millgui.ini");
    let text = std::fs::read_to_string(&ini).map_err(|e| format!("{}: {e}", ini.display()))?;
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("REMOTE_PORT"))
        .map(String::from)
        .collect();
    let at = lines
        .iter()
        .position(|l| l.trim() == "[MILLGUI]")
        .ok_or("prepared millgui.ini has no [MILLGUI] section")?;
    lines.insert(at + 1, format!("REMOTE_PORT = {port}"));
    std::fs::write(&ini, lines.join("\n") + "\n").map_err(|e| e.to_string())
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn run_one(
    env: &Env,
    c: &Config,
    sc: &Scenario,
    out: &Path,
    hd: &Path,
    update_goldens: bool,
) -> Result_ {
    let t0 = Instant::now();
    let dir = out.join(&c.name).join(slug(&sc.name));
    let mut r = Result_ {
        config: c.name.clone(),
        scenario: sc.name.clone(),
        title: sc.title.clone(),
        outcome: Outcome::Pass,
        detail: String::new(),
        secs: 0.0,
        shots: vec![],
        steps: vec![],
        log: None,
    };
    let xfail = sc.xfail.iter().find(|(who, _)| c.tags.contains(who));
    let res = (|| -> Result<(), String> {
        let cfg = dir.join("cfg");
        prepare(c, &cfg, env.port)?;
        let log = dir.join("linuxcnc.log");
        r.log = Some(log.clone());
        for (path, line) in &sc.seeds {
            let p = dir.join("home").join(path);
            std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
            let mut text = std::fs::read_to_string(&p).unwrap_or_default();
            text.push_str(line);
            text.push('\n');
            std::fs::write(&p, text).map_err(|e| e.to_string())?;
        }
        let mut s = Session::start(env, &cfg, log)?;
        if !s.props.lock().unwrap().contains_key("screen") {
            return Err(
                "millgui did not mirror its screen: build lacks harness mode (MILLGUI_HARNESS)"
                    .into(),
            );
        }
        let mut n = 0;
        for line in &sc.steps {
            r.steps
                .push(format!("{}:{}  {}", line.file, line.no, line.text));
            if let Err(e) = step(
                &mut s,
                &line.step,
                &dir,
                &mut n,
                &mut r.shots,
                c,
                sc,
                hd,
                update_goldens,
            ) {
                let shot = dir.join("failure.png");
                if s.screenshot(&shot).is_ok() {
                    r.shots.push(("failure".into(), shot));
                }
                let log = std::fs::read_to_string(&s.log).unwrap_or_default();
                let tail: Vec<&str> = log
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .rev()
                    .take(15)
                    .collect();
                let tail = if tail.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\nlast log lines:\n{}",
                        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                    )
                };
                return Err(format!(
                    "{}:{}  {}\n{e}{tail}",
                    line.file, line.no, line.text
                ));
            }
            if !s.alive() {
                return Err(format!(
                    "{}:{}  LinuxCNC exited after: {}",
                    line.file, line.no, line.text
                ));
            }
        }
        // Any error LinuxCNC or millgui reported fails the scenario unless allowed.
        std::thread::sleep(Duration::from_millis(300));
        let mut errs = s.errors()?;
        errs.extend(s.ui_errors.lock().unwrap().iter().cloned());
        let bad: Vec<_> = errs
            .iter()
            .filter(|e| !sc.allow_errors.iter().any(|a| e.contains(a)))
            .collect();
        if !bad.is_empty() {
            let shot = dir.join("final.png");
            if s.screenshot(&shot).is_ok() {
                r.shots.push(("final".into(), shot));
            }
            return Err(format!(
                "unexpected errors:\n{}",
                bad.iter()
                    .map(|e| format!("  {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        Ok(())
    })();
    r.secs = t0.elapsed().as_secs_f64();
    r.outcome = match (res, xfail) {
        (Ok(()), None) => Outcome::Pass,
        (Ok(()), Some((who, why))) => {
            r.detail = format!("expected to fail on {who} ({why}) but passed: remove the xfail");
            Outcome::XPass
        }
        (Err(e), None) => {
            r.detail = e;
            Outcome::Fail
        }
        (Err(e), Some((who, why))) => {
            r.detail = format!("expected on {who}: {why}\n{e}");
            Outcome::XFail
        }
    };
    r
}

fn eval_source(s: &mut Session, e: &eval::Expr) -> Result<Value, String> {
    let root = match &e.source {
        eval::Source::Stat => s.stat()?,
        eval::Source::Prop => s.prop_snapshot(),
        eval::Source::Hal(pin) => s.hal_get(pin)?,
        eval::Source::Errors => json!(s.errors()?),
        eval::Source::UiErrors => json!(s.ui_errors.lock().unwrap().clone()),
    };
    eval::lookup(&root, &e.path).ok_or_else(|| format!("no value at {:?}", e.path))
}

fn check(s: &mut Session, text: &str) -> Result<(bool, Value), String> {
    let e = eval::parse(text)?;
    let v = eval_source(s, &e);
    match v {
        Ok(v) => Ok((eval::holds(&e, &v), v)),
        Err(err) => Ok((false, Value::String(err))),
    }
}

fn wait_for(s: &mut Session, text: &str, within: f64) -> Result<(), String> {
    let t0 = Instant::now();
    loop {
        let (ok, v) = check(s, text)?;
        if ok {
            return Ok(());
        }
        if t0.elapsed().as_secs_f64() > within {
            return Err(format!(
                "timed out after {within}s; last value: {}",
                short(&v)
            ));
        }
        if !s.alive() {
            return Err("LinuxCNC exited".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn short(v: &Value) -> String {
    let t = v.to_string();
    if t.len() > 300 {
        format!("{}…", &t[..300])
    } else {
        t
    }
}

/// The machine is done: interpreter idle, queue empty, in position.
fn settle(s: &mut Session, within: f64) -> Result<(), String> {
    std::thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    loop {
        let st = s.stat()?;
        if st["interp_state"] == 1 && st["queue"] == 0 && st["inpos"] == true && st["state"] != 2 {
            return Ok(());
        }
        if t0.elapsed().as_secs_f64() > within {
            return Err(format!(
                "machine still busy after {within}s (interp_state {}, state {})",
                st["interp_state"], st["state"]
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[allow(clippy::too_many_arguments)]
fn step(
    s: &mut Session,
    st: &Step,
    dir: &Path,
    n: &mut usize,
    shots: &mut Vec<(String, PathBuf)>,
    c: &Config,
    sc: &Scenario,
    hd: &Path,
    update_goldens: bool,
) -> Result<(), String> {
    match st {
        Step::Do { action, args } => s.action(action, args),
        Step::Set { prop, value } => s.set_prop(prop, value),
        Step::Mdi(text) => {
            s.action("mdi-send", &[Value::String(text.clone())])?;
            settle(s, 120.0)
        }
        Step::Idle(secs) => settle(s, *secs),
        Step::Tap {
            list,
            field,
            value,
            action,
        } => {
            let props = s.prop_snapshot();
            let rows = props
                .get(list)
                .and_then(|v| v.as_array())
                .ok_or_else(|| format!("no list property '{list}'"))?;
            let i = rows
                .iter()
                .position(|r| {
                    // FIELD "-": the row is the value itself (a list of strings).
                    let f = if field == "-" { Some(r) } else { r.get(field) };
                    f.is_some_and(|f| {
                        f.as_str().map(String::from).unwrap_or(f.to_string()) == *value
                    })
                })
                .ok_or_else(|| {
                    format!(
                        "no row of {list} has {field} = {value}; rows: {}",
                        short(&Value::Array(rows.clone()))
                    )
                })?;
            s.action(action, &[json!(i)])
        }
        Step::Screen(id) => {
            s.action("navigate", &[Value::String(id.clone())])?;
            wait_for(s, &format!("prop.screen == \"{id}\""), 5.0)
        }
        Step::Wait { expr, within } => wait_for(s, expr, *within),
        Step::Expect(expr) => {
            // millgui's properties reach the harness a status tick or two
            // after LinuxCNC's: give them a moment, LinuxCNC's none.
            let lag = if expr.trim_start().starts_with("prop.")
                || expr.trim_start().starts_with("ui-errors")
            {
                1.5
            } else {
                0.0
            };
            let t0 = Instant::now();
            loop {
                let (ok, v) = check(s, expr)?;
                if ok {
                    return Ok(());
                }
                if t0.elapsed().as_secs_f64() >= lag {
                    return Err(format!("value was {}", short(&v)));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Step::Hal { pin, value } => s.hal_set(pin, value),
        Step::Sleep(secs) => {
            std::thread::sleep(Duration::from_secs_f64(*secs));
            Ok(())
        }
        Step::Shot(name) => {
            *n += 1;
            let p = dir.join(format!("{:02}-{}.png", n, slug(name)));
            s.screenshot(&p)?;
            shots.push((name.clone(), p));
            Ok(())
        }
        Step::Golden { name, max_pct } => {
            *n += 1;
            let p = dir.join(format!("{:02}-{}.png", n, slug(name)));
            s.screenshot(&p)?;
            shots.push((name.clone(), p.clone()));
            let golden = hd.join("goldens").join(&c.name).join(format!(
                "{}--{}.png",
                slug(&sc.name),
                slug(name)
            ));
            if update_goldens {
                std::fs::create_dir_all(golden.parent().unwrap()).map_err(|e| e.to_string())?;
                std::fs::copy(&p, &golden).map_err(|e| e.to_string())?;
                return Ok(());
            }
            if !golden.exists() {
                return Err(format!(
                    "no golden {} (run with --update-goldens to record it)",
                    golden.display()
                ));
            }
            let (pct, diff) = image::compare(&image::read(&p)?, &image::read(&golden)?)?;
            if pct > *max_pct {
                let dp = dir.join(format!("{:02}-{}-diff.png", n, slug(name)));
                image::write(&dp, &diff)?;
                shots.push((format!("{name} (diff)"), dp));
                return Err(format!(
                    "{pct:.2}% of pixels differ from {} (allowed {max_pct}%)",
                    golden.display()
                ));
            }
            Ok(())
        }
        Step::Log(t) => {
            println!("    {t}");
            Ok(())
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn report(out: &Path, results: &[Result_]) {
    let rel = |p: &Path| p.strip_prefix(out).unwrap_or(p).display().to_string();
    // results.json for tools, report.md for a CI summary, index.html to look at.
    let js: Vec<Value> = results
        .iter()
        .map(|r| json!({"config": r.config, "scenario": r.scenario, "title": r.title, "outcome": r.outcome.label(), "detail": r.detail, "secs": r.secs, "shots": r.shots.iter().map(|(n, p)| json!({"name": n, "path": rel(p)})).collect::<Vec<_>>()}))
        .collect();
    let _ = std::fs::write(
        out.join("results.json"),
        serde_json::to_string_pretty(&js).unwrap_or_default(),
    );

    let mut configs: Vec<&str> = results.iter().map(|r| r.config.as_str()).collect();
    configs.dedup();
    let mut names: Vec<&str> = vec![];
    for r in results {
        if !names.contains(&r.scenario.as_str()) {
            names.push(&r.scenario);
        }
    }
    let mut md = String::from("## millgui harness\n\n| scenario |");
    for c in &configs {
        md += &format!(" {c} |");
    }
    md += "\n|---|";
    md += &"---|".repeat(configs.len());
    md += "\n";
    for n in &names {
        md += &format!("| {n} |");
        for c in &configs {
            let o = results
                .iter()
                .find(|r| r.scenario == *n && r.config == *c)
                .map(|r| r.outcome.label())
                .unwrap_or("");
            let icon = match o {
                "PASS" => "✅ pass",
                "FAIL" => "❌ FAIL",
                "XFAIL" => "➖ xfail",
                "XPASS" => "⚠️ xpass",
                "SKIP" => "· skip",
                _ => "",
            };
            md += &format!(" {icon} |");
        }
        md += "\n";
    }
    for r in results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail | Outcome::XPass))
    {
        md += &format!(
            "\n### {} on {}: {}\n```\n{}\n```\n",
            r.scenario,
            r.config,
            r.outcome.label(),
            r.detail
        );
    }
    let _ = std::fs::write(out.join("report.md"), &md);

    let mut h = String::from(
        "<!doctype html><meta charset=utf-8><title>millgui harness</title><style>\
         body{font:14px system-ui,sans-serif;margin:16px;background:#111;color:#ddd}\
         table{border-collapse:collapse}td,th{border:1px solid #444;padding:4px 8px}\
         .PASS{color:#6c6}.FAIL{color:#f66;font-weight:bold}.XFAIL{color:#aa8}.XPASS{color:#fb4}.SKIP{color:#777}\
         pre{background:#222;padding:8px;white-space:pre-wrap}img{max-width:640px;border:1px solid #444;margin:4px}\
         details{margin:8px 0}a{color:#8bf}</style><h1>millgui harness</h1><table><tr><th>scenario</th>",
    );
    for c in &configs {
        h += &format!("<th>{}</th>", esc(c));
    }
    h += "</tr>";
    for n in &names {
        h += &format!("<tr><td>{}</td>", esc(n));
        for c in &configs {
            match results.iter().find(|r| r.scenario == *n && r.config == *c) {
                Some(r) => {
                    h += &format!(
                        "<td class={0}><a href='#{1}--{2}'>{0}</a></td>",
                        r.outcome.label(),
                        slug(c),
                        slug(n)
                    )
                }
                None => h += "<td></td>",
            }
        }
        h += "</tr>";
    }
    h += "</table>";
    for r in results.iter().filter(|r| r.outcome != Outcome::Skip) {
        h += &format!(
            "<details id='{}--{}' {}><summary><span class={}>{}</span> {} on {} — {} ({:.1}s)</summary>",
            slug(&r.config),
            slug(&r.scenario),
            if r.outcome == Outcome::Fail { "open" } else { "" },
            r.outcome.label(),
            r.outcome.label(),
            esc(&r.scenario),
            esc(&r.config),
            esc(&r.title),
            r.secs
        );
        if !r.detail.is_empty() {
            h += &format!("<pre>{}</pre>", esc(&r.detail));
        }
        if let Some(l) = &r.log {
            h += &format!("<p><a href='{}'>linuxcnc.log</a></p>", esc(&rel(l)));
        }
        for (name, p) in &r.shots {
            h += &format!("<figure style='display:inline-block'><a href='{0}'><img src='{0}'></a><figcaption>{1}</figcaption></figure>", esc(&rel(p)), esc(name));
        }
        h += "</details>";
    }
    let _ = std::fs::write(out.join("index.html"), h);
}
