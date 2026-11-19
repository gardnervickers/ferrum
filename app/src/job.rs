//! JOB screen (milestone 6): the open job, its build, and the `mymill-job`
//! HAL pins the built program reads.
//!
//! The rules live in the jobcore crate (a port of mymill_vcp/job_session.py
//! and mymill_job, checked against their output): one JobSession answering
//! JSON requests in-process. This side shows its snapshots, loads the
//! programs it builds, copies its pins to HAL, and asks it before CYCLE
//! START starts a program.

use crate::config::Config;
use crate::{Actions, JobOp, Live, MainWindow};
use machine::{Command, Handle, Interp, Status};
use serde_json::{json, Value};
use slint::{ComponentHandle, SharedString};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Must match mymill_job.model.MAX_OPS (one skip pin per position).
const MAX_OPS: usize = 64;
/// Manual step presets (job_session.STEP_PRESETS).
const STEP_PRESETS: [&str; 5] = [
    "FLIP PART AND RE-CLAMP",
    "CLEAR CHIPS AND CHECK THE PART",
    "MEASURE THE PART",
    "CHANGE VISE JAWS",
    "MOVE CLAMPS CLEAR OF THE NEXT OP",
];

/// The job session (jobcore), one request and reply at a time.
struct Helper(jobcore::Helper);

impl Helper {
    fn start(dir: &std::path::Path) -> Result<Self, String> {
        Ok(Self(jobcore::Helper::new(dir.to_path_buf())))
    }

    fn call(&mut self, req: &Value) -> Result<Value, String> {
        Ok(self.0.handle(req))
    }
}

/// The component the built program reads (`#<_hal[mymill-job.stamp]>`).
struct Pins {
    _comp: lcnc::hal::Component,
    stamp: lcnc::hal::Out<i32>,
    skip: Vec<lcnc::hal::Out<bool>>,
    stop_after: lcnc::hal::Out<bool>,
}

impl Pins {
    fn new() -> Result<Self, String> {
        let comp = lcnc::hal::Component::new("mymill-job")?;
        let stamp = comp.pin("stamp")?;
        let skip = (1..=MAX_OPS).map(|p| comp.pin(&format!("skip-{p}"))).collect::<Result<Vec<_>, _>>()?;
        let stop_after = comp.pin("stop-after")?;
        comp.ready()?;
        Ok(Self { _comp: comp, stamp, skip, stop_after })
    }

    fn write(&self, pins: &Value) {
        self.stamp.set(pins["stamp"].as_i64().unwrap_or(0) as i32);
        let on: Vec<u64> = pins["skip"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64()).collect()).unwrap_or_default();
        for (i, p) in self.skip.iter().enumerate() {
            p.set(on.contains(&(i as u64 + 1)));
        }
        self.stop_after.set(pins["stop_after"].as_bool().unwrap_or(false));
    }
}

/// What the shared list picker is open for.
enum Pick {
    Open(Vec<String>),
    /// The step presets after this many tool check entries.
    Step(usize),
    Program(Vec<PathBuf>),
    /// Which earlier op a check measures (None: no op).
    Link(Vec<Option<String>>),
}

pub struct Job {
    helper: Option<Helper>,
    init: Value,
    dir: PathBuf,
    /// Why the helper is not running, shown on the JOB page.
    down: Option<String>,
    snap: Value,
    selected: Option<String>,
    /// A message from the last button press; stays until the next one.
    pinned: Option<(String, String)>,
    status: Option<Status>,
    polls: u32,
    /// The backplot's toolpath when the page was last shown (its op times).
    plot_id: u64,
    pick: Option<Pick>,
    pins: Option<Pins>,
    /// The last reading of each check op this session (op id -> value),
    /// from the job program's MYMILL-MEASURE lines.
    readings: HashMap<String, f64>,
    /// The check measured while it is the running op (its id): that reading
    /// is from this run, so the running row may show it.
    fresh: Option<String>,
    /// Checks whose last reading already corrected a tool (not offered again).
    corrected: std::collections::HashSet<String>,
    machine: Handle,
    ui: slint::Weak<MainWindow>,
    /// The machine has a tool setter: ADD STEP offers tool checks.
    has_setter: bool,
}

impl Job {
    /// Send one request. Restarts a helper that died; returns its reply.
    pub fn call(&mut self, req: Value) -> Result<Value, String> {
        if self.helper.is_none() {
            let mut h = Helper::start(&self.dir).map_err(|e| self.set_down(e))?;
            let r = h.call(&self.init).map_err(|e| self.set_down(e))?;
            if r["ok"] != json!(true) {
                return Err(self.set_down(r["error"].as_str().unwrap_or("job helper failed").into()));
            }
            self.down = None;
            self.helper = Some(h);
            self.take(&r);
        }
        let r = match self.helper.as_mut().unwrap().call(&req) {
            Ok(r) => r,
            Err(e) => {
                self.helper = None;
                return Err(self.set_down(e));
            }
        };
        self.take(&r);
        if r["ok"] == json!(true) {
            Ok(r)
        } else {
            Err(r["error"].as_str().unwrap_or("job helper error").to_string())
        }
    }

    fn set_down(&mut self, e: String) -> String {
        self.down = Some(e.clone());
        e
    }

    /// Keep a reply's snapshot, copy its pins, load what it built.
    fn take(&mut self, r: &Value) {
        if let Some(s) = r.get("snap") {
            self.snap = s.clone();
            // Another op (or none) is running now: a later run of the same
            // check starts without a reading.
            let current = self.ops().into_iter().find(|o| o["state"] == "current").and_then(|o| o["id"].as_str().map(String::from));
            if self.fresh.is_some() && self.fresh != current {
                self.fresh = None;
            }
            if let Some(p) = &self.pins {
                p.write(&s["pins"]);
            }
        }
        if let Some(path) = r["load"].as_str() {
            self.machine.send(Command::ProgramOpen(path.into()));
        }
    }

    pub fn tool_table(&self) -> PathBuf {
        PathBuf::from(self.init["tool_table"].as_str().unwrap_or("tool.tbl"))
    }

    pub fn tool_diameter(&self, t: i32) -> f64 {
        let tools = std::fs::read_to_string(self.tool_table()).map(|s| crate::tools::parse(&s)).unwrap_or_default();
        tools.iter().find(|x| x.t == t).map_or(0.0, |x| x.d)
    }

    fn stat(&self) -> Value {
        match &self.status {
            Some(s) => json!({
                "file": s.file, "idle": s.interp == Interp::Idle, "motion_line": s.motion_line,
                "current_line": s.current_line, "paused": s.interp == Interp::Paused,
            }),
            None => json!({}),
        }
    }

    /// Why CYCLE START must not start the loaded program, if it is a job.
    pub fn cycle_start_problem(&mut self, line: i32) -> Option<String> {
        let req = json!({"cmd": "cycle_start", "stat": self.stat(), "line": line});
        match self.call(req) {
            Ok(r) => r["problem"].as_str().map(str::to_string),
            // A job program checks its stamp itself; a helper fault must not
            // block ordinary programs.
            Err(_) => None,
        }
    }

    fn act(&mut self, req: Value) {
        self.pinned = None;
        match self.call(req) {
            Ok(r) => {
                if let Some(m) = r["message"].as_str() {
                    self.pinned = Some((m.to_string(), r["tone"].as_str().unwrap_or("").to_string()));
                }
            }
            Err(e) => self.pinned = Some((e, "error".into())),
        }
        self.refresh();
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_deref()?;
        self.ops().iter().position(|o| o["id"].as_str() == Some(id))
    }

    fn ops(&self) -> Vec<Value> {
        self.snap["ops"].as_array().cloned().unwrap_or_default()
    }

    /// The job step running now, by name.
    pub fn current_op(&self) -> Option<String> {
        if !self.running() {
            return None;
        }
        self.ops().into_iter().find(|o| o["state"] == "current").and_then(|o| o["name"].as_str().map(String::from))
    }

    fn running(&self) -> bool {
        self.snap["running"].as_bool().unwrap_or(false)
    }

    pub fn refresh(&self) {
        if let Some(ui) = self.ui.upgrade() {
            self.show(&ui);
        }
    }

    fn show(&self, ui: &MainWindow) {
        let live = ui.global::<Live>();
        let s = &self.snap;
        let running = self.running();
        let ops = self.ops();
        let sel = if running { None } else { self.selected_index() };
        let position = s["position"].as_i64().unwrap_or(0);
        // Run-time estimates from the loaded program, when it is this job's
        // build (its ops carry the job's op ids).
        let off: std::collections::HashSet<String> =
            ops.iter().filter(|o| o["enabled"] == json!(false)).filter_map(|o| o["id"].as_str().map(|id| id.to_uppercase())).collect();
        let times = crate::plot::with_ret(|p| {
            crate::plot::set_skip(p, &off);
            (crate::plot::job_times(p), crate::plot::run_left(p))
        });
        let (times, run_left) = match times {
            Some((t, l)) => (Some(t), l),
            None => (None, None),
        };
        // The program's markers carry the ids in capitals.
        let op_time = |o: &Value| times.as_ref().and_then(|t| t.by_id.get(&o["id"].as_str()?.to_uppercase()).copied()).filter(|s| *s > 0.0);
        let now = |o: &Value| times.as_ref().and_then(|t| t.now.clone()).filter(|n| o["id"].as_str().is_some_and(|id| id.eq_ignore_ascii_case(&n.0)));
        let rows: Vec<JobOp> = ops
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let state = o["state"].as_str().unwrap_or("idle");
                JobOp {
                    pos: o["pos"].as_i64().unwrap_or(0).to_string().into(),
                    badge: o["badge"].as_str().unwrap_or("").into(),
                    kind: o["kind"].as_str().unwrap_or("").into(),
                    name: o["name"].as_str().unwrap_or("").into(),
                    detail: {
                        let d = o["detail"].as_str().unwrap_or("");
                        let d = match o["id"].as_str().and_then(|id| self.readings.get(id)) {
                            Some(v) => format!("LAST {}  ·  {d}", reading_text(*v, &o["probe"])),
                            None => d.to_string(),
                        };
                        // The op's estimated run time first, so it never elides.
                        match op_time(o).filter(|_| state != "done" && state != "skipped") {
                            Some(t) => format!("{}  ·  {d}", crate::plot::fmt_clock(t)),
                            None => d,
                        }
                        .into()
                    },
                    enabled: o["enabled"].as_bool().unwrap_or(true),
                    state: state.into(),
                    can_toggle: o["can_toggle"].as_bool().unwrap_or(false),
                    selected: sel == Some(i),
                    // By time when the run is timed, else by line.
                    progress: if state == "current" { now(o).map_or(s["op_fraction"].as_f64().unwrap_or(0.0), |n| n.1) as f32 } else { 0.0 },
                    progress_text: if state == "current" {
                        let pct = match now(o) {
                            Some((_, frac, left)) => format!("{:.0}%  ·  {} LEFT", frac * 100.0, crate::plot::fmt_clock(left)),
                            None => format!("{:.0}%", s["op_fraction"].as_f64().unwrap_or(0.0) * 100.0),
                        };
                        let p = s["progress"].as_str().unwrap_or("");
                        let text = if p.is_empty() { pct } else { format!("{pct}  ·  {p}") };
                        // A check that pauses out of tolerance: the reading
                        // is what CYCLE START or STOP is decided on.
                        let id = o["id"].as_str();
                        match id.filter(|id| self.fresh.as_deref() == Some(id)).and_then(|id| self.readings.get(id)) {
                            Some(v) => format!("{text}  ·  MEASURED {}", reading_text(*v, &o["probe"])),
                            None => text,
                        }
                        .into()
                    } else {
                        SharedString::new()
                    },
                    stop_after: state == "current" && s["stop_requested"].as_bool().unwrap_or(false) && position > 0,
                }
            })
            .collect();
        let n = ops.len();
        crate::models::sync(live.get_job_ops(), rows, |m| live.set_job_ops(m));
        live.set_job_selected(sel.map_or(-1, |i| i as i32));
        live.set_job_title(match &self.down {
            Some(_) if self.snap.is_null() => "JOB".into(),
            _ => {
                // The whole job's time: what is left while it runs, else the
                // estimate of the ops it includes.
                let time = match (running, run_left) {
                    (true, Some(l)) => format!("  ·  {} LEFT", crate::plot::fmt_clock(l)),
                    _ => {
                        let total: f64 = ops.iter().filter(|o| o["enabled"] != json!(false)).filter_map(op_time).sum();
                        if total > 0.0 { format!("  ·  {}", crate::plot::fmt_clock(total)) } else { String::new() }
                    }
                };
                format!("{}  ·  {} OP{}{time}", s["name"].as_str().unwrap_or("JOB"), n, if n == 1 { "" } else { "S" }).into()
            }
        });
        live.set_job_state(s["state"].as_str().unwrap_or("NOT BUILT").into());
        live.set_job_tone(s["tone"].as_str().unwrap_or("").into());
        live.set_job_running(running);
        live.set_job_fraction(s["job_fraction"].as_f64().unwrap_or(0.0) as f32);
        live.set_job_stop_requested(s["stop_requested"].as_bool().unwrap_or(false));
        let (note, tone) = if let Some(d) = &self.down {
            (d.clone(), "error".to_string())
        } else if let (Some((m, t)), false) = (&self.pinned, running) {
            (m.clone(), t.clone())
        } else {
            (s["note"].as_str().unwrap_or("").to_string(), s["note_tone"].as_str().unwrap_or("").to_string())
        };
        let note = if note.is_empty() && n == 0 {
            "No ops yet. Use ADD PROGRAM for a Fusion program, ADD TO JOB on the CONV or PROBE page, or ADD STEP.".to_string()
        } else {
            note
        };
        live.set_job_note(note.into());
        live.set_job_note_tone(tone.into());
        self.show_check(ui, sel.and_then(|i| ops.get(i)));
    }

    /// The check strip for a selected MEASURE op.
    fn show_check(&self, ui: &MainWindow, op: Option<&Value>) {
        let live = ui.global::<Live>();
        let p = op.map(|o| &o["probe"]).filter(|p| p["mode"] == "measure");
        live.set_job_check(p.is_some());
        let Some(p) = p else { return };
        let id = op.and_then(|o| o["id"].as_str()).unwrap_or("");
        live.set_job_check_value(value_name(p["value"].as_str().unwrap_or("")).into());
        live.set_job_check_values(p["values"].as_array().map_or(0, |a| a.len()) as i32);
        let u = crate::machine_info::get().units;
        live.set_job_check_nominal(p["nominal"].as_f64().map_or("NONE".into(), |v| u.fmt(v)).into());
        live.set_job_check_tol(format!("±{}", u.fmt(p["tol"].as_f64().unwrap_or(0.0))).into());
        live.set_job_check_op(match (p["op_pos"].as_i64(), p["op_name"].as_str()) {
            (Some(n), Some(name)) => format!("OP {n} · {name}"),
            _ => "NO OP".into(),
        }.into());
        live.set_job_check_pause(p["pause"].as_bool().unwrap_or(true));
        let (last, tone) = match self.readings.get(id) {
            Some(v) => {
                let ok = match p["nominal"].as_f64() {
                    Some(n) => (v - n).abs() <= p["tol"].as_f64().unwrap_or(0.0) + 1e-9,
                    None => true,
                };
                let fixed = if self.corrected.contains(id) { "  ·  TOOL CORRECTED" } else { "" };
                (format!("LAST {}{fixed}", reading_text(*v, p)), if ok { "good" } else { "bad" })
            }
            None => ("NOT MEASURED YET".to_string(), ""),
        };
        live.set_job_check_last(last.into());
        live.set_job_check_tone(tone.into());
        live.set_job_check_adjust(self.adjustment(id).map_or(String::new(), |a| a.2).into());
    }

    /// For the selected check: (tool, new diameter, button text) when its
    /// last reading suggests a diameter change for the tool that cut it.
    pub fn adjustment(&self, id: &str) -> Option<(i32, f64, String)> {
        let op = self.ops().into_iter().find(|o| o["id"].as_str() == Some(id))?;
        let p = &op["probe"];
        if p["sized"] != json!(true) || self.corrected.contains(id) {
            return None;
        }
        let (v, n) = (*self.readings.get(id)?, p["nominal"].as_f64()?);
        // No correction from a routine that left no result (the −999999
        // placeholder), or from an error too big to be tool wear: that is a
        // wrong nominal, the wrong feature or a bad probe, not the tool.
        let u = crate::machine_info::get().units;
        if (v - n).abs() < u.from_mm(0.0005) || (v - n).abs() > u.from_mm(MAX_CORRECTION) {
            return None;
        }
        let (t, d) = (p["tool"].as_i64()? as i32, p["diameter"].as_f64()?);
        let new = jobcore::probe::corrected_diameter(p["routine"].as_str()?, d, v, n)?;
        if !(new > 0.0) {
            return None;
        }
        Some((t, new, format!("SET T{t} Ø {} → {}", u.fmt(d), u.fmt(new))))
    }

    /// The selected check's reading has been used to correct its tool.
    pub fn mark_corrected(&mut self, id: &str) {
        self.corrected.insert(id.to_string());
        self.refresh();
    }

    pub fn selected_id(&self) -> Option<String> {
        self.selected.clone()
    }

    /// Add a probing op made on the PROBE screen.
    pub fn add_probe(&mut self, name: &str, params: Value) -> Result<String, String> {
        let r = self.call(json!({"cmd": "add_probe", "name": name, "params": params}))?;
        let m = r["message"].as_str().unwrap_or("Added").to_string();
        self.pinned = Some((m.clone(), "ready".into()));
        self.refresh();
        Ok(m)
    }

    fn check_set(&mut self, changes: Value) {
        let Some(id) = self.selected.clone() else { return };
        self.act(json!({"cmd": "set_measure", "id": id, "changes": changes}));
    }

    fn check_probe(&self) -> Option<Value> {
        let id = self.selected.as_deref()?;
        self.ops().into_iter().find(|o| o["id"].as_str() == Some(id)).map(|o| o["probe"].clone()).filter(|p| p["mode"] == "measure")
    }

    /// A `MYMILL-MEASURE ID=<id> VALUE=<v>` line from a running job.
    pub fn message(&mut self, text: &str) -> bool {
        let Some(rest) = text.trim().strip_prefix("MYMILL-MEASURE ") else { return false };
        let mut id = None;
        let mut value = None;
        for w in rest.split_whitespace() {
            if let Some(v) = w.strip_prefix("ID=") {
                id = Some(v.to_string());
            } else if let Some(v) = w.strip_prefix("VALUE=") {
                value = v.parse::<f64>().ok();
            }
        }
        if let (Some(id), Some(v)) = (id, value) {
            self.corrected.remove(&id);
            self.fresh = Some(id.clone());
            self.readings.insert(id, v);
            self.refresh();
        }
        true
    }

    fn open_pick(&mut self, title: &str, items: Vec<String>, pick: Pick) {
        if let Some(ui) = self.ui.upgrade() {
            crate::picker::open(&ui, "job", title, items, None);
        }
        self.pick = Some(pick);
    }
}

/// The largest size error (mm; converted on an inch machine) a check offers
/// to fix as tool wear.
const MAX_CORRECTION: f64 = 0.5;

fn value_name(v: &str) -> &'static str {
    match v {
        "diameter" => "DIAMETER",
        "x" => "X WIDTH",
        "y" => "Y WIDTH",
        _ => "Z HEIGHT",
    }
}

/// "9.968 (−0.032)" against the check's nominal, or the bare value.
fn reading_text(v: f64, probe: &Value) -> String {
    if v <= -999998.0 {
        return "NO RESULT".into();
    }
    // The machine's decimals (3 in mm, 4 in inches) without a "-0.000".
    let u = crate::machine_info::get().units;
    let v = u.round(v);
    match probe["nominal"].as_f64() {
        Some(n) => {
            let d = u.round(v - n);
            let sign = if d < 0.0 { "−" } else { "+" };
            format!("{} ({sign}{})", u.fmt(v), u.fmt(d.abs()))
        }
        None => u.fmt(v),
    }
}

/// Keypad values of the check strip: "jc-nominal", "jc-tol".
struct Keys(Rc<RefCell<Job>>);

impl crate::manual::KeypadTarget for Keys {
    fn label(&self, what: &str) -> Option<String> {
        match what {
            "jc-nominal" => Some(format!("CHECK · NOMINAL {}", crate::machine_info::get().units.upper())),
            "jc-tol" => Some(format!("CHECK · TOLERANCE ± {}", crate::machine_info::get().units.upper())),
            _ => None,
        }
    }
    fn current(&self, what: &str) -> Option<f64> {
        let p = self.0.borrow().check_probe()?;
        match what {
            "jc-nominal" => Some(p["nominal"].as_f64().unwrap_or(0.0)),
            "jc-tol" => p["tol"].as_f64(),
            _ => None,
        }
    }
    fn apply(&self, what: &str, v: f64) -> Option<Result<(), String>> {
        let key = match what {
            "jc-nominal" => "nominal",
            "jc-tol" => "tol",
            _ => return None,
        };
        if key == "tol" && !(v > 0.0) {
            return Some(Err("The tolerance must be more than 0".into()));
        }
        self.0.borrow_mut().check_set(json!({ key: v }));
        Some(Ok(()))
    }
}

/// Status: follow the controller (run state, op progress).
pub fn apply_status(j: &Rc<RefCell<Job>>, s: &Status) {
    j.borrow_mut().status = Some(s.clone());
}

/// Is `path` the folder job builds go in (or a program in it)?
pub fn folder(cfg: &Config, sim: bool) -> PathBuf {
    if sim {
        // Under the sim's FILES folder, as jobs/ is under PROGRAM_PREFIX.
        cache_dir().join("sim-nc").join("jobs")
    } else {
        crate::files::root(cfg).join("jobs")
    }
}

pub fn cache_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(".cache/ferrum")
}

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, sim: bool, has_setter: bool) -> (Rc<RefCell<Job>>, slint::Timer) {
    let num = |s, k| cfg.get_f64(s, k).map_or(Value::Null, |v| json!(v));
    let mi = crate::machine_info::get();
    let init = json!({
        "cmd": "init",
        "folder": folder(cfg, sim),
        "nc_folder": if sim { cache_dir().join("sim-nc") } else { crate::files::root(cfg) },
        "tool_table": crate::tools::table_path(cfg, sim),
        "probe_tool": crate::machine_cfg::get().probe_tool(cfg) as i64,
        "max_forward_rpm": mi.max_rpm.map_or(Value::Null, |v| json!(v)),
        "max_reverse_rpm": if mi.reverse { num("SPINDLE_0", "MAX_REVERSE_VELOCITY") } else { json!(0.0) },
        // The tool table and probe settings are in these; ops may be in either.
        "units": mi.units.upper(),
    });
    let init_nc = init["nc_folder"].as_str().unwrap_or_default().to_string();
    let mut pins = None;
    let mut down = None;
    if !sim {
        match Pins::new() {
            Ok(p) => pins = Some(p),
            Err(e) => down = Some(format!("HAL mymill-job: {e}")),
        }
    }
    let j = Rc::new(RefCell::new(Job {
        helper: None,
        init,
        dir: cfg.dir().to_path_buf(),
        down: None,
        snap: Value::Null,
        selected: None,
        pinned: None,
        status: None,
        polls: 0,
        plot_id: 0,
        pick: None,
        pins,
        readings: HashMap::new(),
        fresh: None,
        corrected: Default::default(),
        machine: machine.clone(),
        ui: ui.as_weak(),
        has_setter,
    }));
    {
        let mut jb = j.borrow_mut();
        // Starts the helper, which reopens the last job.
        let _ = jb.call(json!({"cmd": "files"}));
        if let Some(d) = down {
            jb.pinned = Some((d, "error".into()));
        }
        jb.show(ui);
    }

    crate::manual::add_keypad_target(Rc::new(Keys(j.clone())));
    let actions = ui.global::<Actions>();
    let t = j.clone();
    actions.on_job_check_link(move || {
        let mut jb = t.borrow_mut();
        let Some(id) = jb.selected.clone() else { return };
        // Ops before the check that cut something.
        let mut names = vec!["NO OP".to_string()];
        let mut ids = vec![None];
        for o in jb.ops() {
            if o["id"].as_str() == Some(id.as_str()) {
                break;
            }
            if matches!(o["kind"].as_str(), Some("conversational") | Some("cam")) {
                names.push(format!("OP {} · {}", o["pos"].as_i64().unwrap_or(0), o["name"].as_str().unwrap_or("").to_uppercase()));
                ids.push(o["id"].as_str().map(str::to_string));
            }
        }
        jb.open_pick("WHICH OP CUT THIS FEATURE?", names, Pick::Link(ids));
    });
    let t = j.clone();
    actions.on_job_check_value(move || {
        let mut jb = t.borrow_mut();
        let Some(p) = jb.check_probe() else { return };
        let values: Vec<&str> = p["values"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
        let at = values.iter().position(|v| Some(*v) == p["value"].as_str()).unwrap_or(0);
        if let Some(next) = values.get((at + 1) % values.len().max(1)) {
            jb.check_set(json!({"value": next}));
        }
    });
    let t = j.clone();
    actions.on_job_check_pause(move || {
        let mut jb = t.borrow_mut();
        let Some(p) = jb.check_probe() else { return };
        let on = !p["pause"].as_bool().unwrap_or(true);
        jb.check_set(json!({"on_fail": if on { "pause" } else { "continue" }}));
    });
    let t = j.clone();
    actions.on_job_select(move |i| {
        let mut jb = t.borrow_mut();
        if jb.running() {
            return;
        }
        let id = jb.ops().get(i.max(0) as usize).and_then(|o| o["id"].as_str().map(str::to_string));
        jb.selected = if jb.selected == id { None } else { id };
        jb.refresh();
    });
    let t = j.clone();
    actions.on_job_toggle(move |i| {
        let mut jb = t.borrow_mut();
        let Some(o) = jb.ops().get(i.max(0) as usize).cloned() else { return };
        let on = !o["enabled"].as_bool().unwrap_or(true);
        jb.act(json!({"cmd": "set_enabled", "id": o["id"], "enabled": on}));
    });
    let t = j.clone();
    actions.on_job_new(move || {
        let mut jb = t.borrow_mut();
        jb.selected = None;
        jb.act(json!({"cmd": "new"}));
    });
    let t = j.clone();
    actions.on_job_open(move || {
        let mut jb = t.borrow_mut();
        jb.pinned = None;
        match jb.call(json!({"cmd": "files"})) {
            Ok(r) => {
                let names: Vec<String> = r["names"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                let paths: Vec<String> = r["paths"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                if names.is_empty() {
                    jb.pinned = Some(("No saved jobs yet.".into(), "warn".into()));
                } else {
                    jb.open_pick("OPEN JOB", names, Pick::Open(paths));
                }
            }
            Err(e) => jb.pinned = Some((e, "error".into())),
        }
        jb.refresh();
    });
    let t = j.clone();
    actions.on_job_add_step(move || {
        let mut jb = t.borrow_mut();
        // Tool checks first (with a tool setter), then the manual steps.
        let mut items = Vec::new();
        if jb.has_setter {
            items.push("TOOL CHECK AFTER EVERY TOOL (BROKEN TOOL CHECK)".to_string());
            let after = jb.selected_index().map(|i| format!("TOOL CHECK AFTER OP {}", i + 1)).unwrap_or_else(|| "TOOL CHECK AT THE END".into());
            items.push(after);
        }
        let n = items.len();
        items.extend(STEP_PRESETS.iter().map(|s| s.to_string()));
        jb.open_pick("ADD A STEP", items, Pick::Step(n));
    });
    let t = j.clone();
    let nc = PathBuf::from(init_nc);
    actions.on_job_add_program(move || {
        let programs = cam_programs(&nc);
        let mut jb = t.borrow_mut();
        if programs.is_empty() {
            jb.pinned = Some((format!("No programs in {}.", nc.display()), "error".into()));
            jb.refresh();
            return;
        }
        let names = programs.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
        jb.open_pick("ADD A FUSION PROGRAM", names, Pick::Program(programs));
    });
    let t = j.clone();
    actions.on_job_move(move |dir| {
        let mut jb = t.borrow_mut();
        if let Some(id) = jb.selected.clone() {
            jb.act(json!({"cmd": "move", "id": id, "offset": dir}));
        }
    });
    let t = j.clone();
    actions.on_job_delete(move || {
        let mut jb = t.borrow_mut();
        if let Some(id) = jb.selected.take() {
            jb.act(json!({"cmd": "remove", "id": id}));
        }
    });
    let t = j.clone();
    actions.on_job_start_at(move || {
        let mut jb = t.borrow_mut();
        if let Some(id) = jb.selected.clone() {
            jb.act(json!({"cmd": "start_at", "id": id}));
        }
    });
    let t = j.clone();
    actions.on_job_stop_after(move || {
        let mut jb = t.borrow_mut();
        let on = !jb.snap["stop_requested"].as_bool().unwrap_or(false);
        jb.act(json!({"cmd": "stop_after", "on": on}));
    });
    let t = j.clone();
    actions.on_job_build(move || {
        let mut jb = t.borrow_mut();
        let busy = jb.status.as_ref().is_some_and(|s| s.interp != Interp::Idle);
        if busy {
            jb.pinned = Some(("Stop the running program before building.".into(), "error".into()));
            jb.refresh();
            return;
        }
        jb.act(json!({"cmd": "build"}));
    });
    let t = j.clone();
    crate::picker::on_pick("job", move |i| {
        let mut jb = t.borrow_mut();
        match jb.pick.take() {
            Some(Pick::Open(paths)) => {
                if let Some(p) = paths.get(i) {
                    jb.selected = None;
                    jb.act(json!({"cmd": "open", "path": p}));
                }
            }
            Some(Pick::Step(n)) if n > 0 && i == 0 => jb.act(json!({"cmd": "add_tool_checks"})),
            Some(Pick::Step(n)) if i < n => {
                let after = jb.selected.clone();
                jb.act(json!({"cmd": "add_tool_check", "after": after}));
            }
            Some(Pick::Step(n)) => jb.act(json!({"cmd": "add_step", "index": i - n})),
            Some(Pick::Program(paths)) => {
                if let Some(p) = paths.get(i) {
                    jb.act(json!({"cmd": "import_cam", "path": p}));
                }
            }
            Some(Pick::Link(ids)) => {
                if let Some(id) = ids.get(i) {
                    jb.check_set(json!({"op": id}));
                }
            }
            None => {}
        }
    });

    // Follow the controller. Publishing re-reads the tool table, so a table
    // change withdraws the stamp within a second.
    let timer = slint::Timer::default();
    let t = j.clone();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        let mut jb = t.borrow_mut();
        if jb.status.is_none() || (jb.helper.is_none() && jb.down.is_some()) {
            return;
        }
        jb.polls += 1;
        let req = json!({"cmd": "poll", "stat": jb.stat(), "publish": jb.polls % 4 == 0});
        let before = jb.snap.clone();
        // While running, the time left moves on even when the snapshot
        // doesn't; a newly read program brings its op times.
        let plot_id = crate::plot::with_ret(|p| crate::plot::toolpath_id(p)).unwrap_or(0);
        if jb.call(req).is_ok() && (jb.snap != before || jb.running() || plot_id != jb.plot_id) {
            jb.plot_id = plot_id;
            jb.refresh();
        }
    });
    (j, timer)
}

/// G-code programs directly in the programs folder (not built jobs), by name.
fn cam_programs(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["ngc", "nc", "tap"].contains(&e.to_ascii_lowercase().as_str()))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}
