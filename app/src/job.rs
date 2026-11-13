//! JOB screen (milestone 6): the open job, its build, and the `mymill-job`
//! HAL pins the built program reads.
//!
//! The rules live in Python (mymill_vcp/job_session.py, shared with the Probe
//! Basic JOB page): millgui/job_helper.py hosts one JobSession and answers
//! JSON lines. This side shows its snapshots, loads the programs it builds,
//! copies its pins to HAL, and asks it before CYCLE START starts a program.

use crate::config::Config;
use crate::{Actions, JobOp, Live, MainWindow};
use linuxcnc::{Command, Handle, Interp, Status};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as Proc, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
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

/// The Python child process. One request, one reply, in order.
struct Helper {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
}

impl Helper {
    fn start(dir: &Path) -> Result<Self, String> {
        let mut child = Proc::new("python3")
            .arg("-u")
            .arg("millgui/job_helper.py")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("job helper: python3: {e}"))?;
        let stdin = child.stdin.take().ok_or("job helper: no stdin")?;
        let stdout = child.stdout.take().ok_or("job helper: no stdout")?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self { child, stdin, rx })
    }

    fn call(&mut self, req: &Value) -> Result<Value, String> {
        writeln!(self.stdin, "{req}").and_then(|_| self.stdin.flush()).map_err(|e| format!("job helper: {e}"))?;
        let line = self.rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "The job helper stopped answering".to_string())?;
        serde_json::from_str(&line).map_err(|e| format!("job helper: bad reply: {e}"))
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The component the built program reads (`#<_hal[mymill-job.stamp]>`).
struct Pins {
    _comp: hal::Component,
    stamp: hal::S32Pin,
    skip: Vec<hal::BitPin>,
    stop_after: hal::BitPin,
}

impl Pins {
    fn new() -> Result<Self, String> {
        let comp = hal::Component::new("mymill-job")?;
        let stamp = comp.s32("stamp", hal::Dir::Out)?;
        let skip = (1..=MAX_OPS).map(|p| comp.bit(&format!("skip-{p}"), hal::Dir::Out)).collect::<Result<Vec<_>, _>>()?;
        let stop_after = comp.bit("stop-after", hal::Dir::Out)?;
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
    Step,
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
    pick: Option<Pick>,
    pins: Option<Pins>,
    machine: Handle,
    ui: slint::Weak<MainWindow>,
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
            if let Some(p) = &self.pins {
                p.write(&s["pins"]);
            }
        }
        if let Some(path) = r["load"].as_str() {
            self.machine.send(Command::ProgramOpen(path.to_string()));
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
                    detail: o["detail"].as_str().unwrap_or("").into(),
                    enabled: o["enabled"].as_bool().unwrap_or(true),
                    state: state.into(),
                    can_toggle: o["can_toggle"].as_bool().unwrap_or(false),
                    selected: sel == Some(i),
                    progress: if state == "current" { s["op_fraction"].as_f64().unwrap_or(0.0) as f32 } else { 0.0 },
                    progress_text: if state == "current" {
                        let pct = format!("{:.0}%", s["op_fraction"].as_f64().unwrap_or(0.0) * 100.0);
                        let p = s["progress"].as_str().unwrap_or("");
                        if p.is_empty() { pct } else { format!("{pct}  ·  {p}") }.into()
                    } else {
                        SharedString::new()
                    },
                    stop_after: state == "current" && s["stop_requested"].as_bool().unwrap_or(false) && position > 0,
                }
            })
            .collect();
        let n = ops.len();
        live.set_job_ops(ModelRc::new(VecModel::from(rows)));
        live.set_job_selected(sel.map_or(-1, |i| i as i32));
        live.set_job_title(match &self.down {
            Some(_) if self.snap.is_null() => "JOB".into(),
            _ => format!("{}  ·  {} OP{}", s["name"].as_str().unwrap_or("JOB"), n, if n == 1 { "" } else { "S" }).into(),
        });
        live.set_job_state(s["state"].as_str().unwrap_or("NOT BUILT").into());
        live.set_job_tone(s["tone"].as_str().unwrap_or("").into());
        live.set_job_running(running);
        live.set_job_fraction(s["job_fraction"].as_f64().unwrap_or(0.0) as f32);
        live.set_job_stop_requested(s["stop_requested"].as_bool().unwrap_or(false));
        let (note, tone) = if let Some(d) = &self.down {
            (format!("{d}. The JOB page needs python3 and the mymill_job package in the config folder."), "error".to_string())
        } else if let (Some((m, t)), false) = (&self.pinned, running) {
            (m.clone(), t.clone())
        } else {
            (s["note"].as_str().unwrap_or("").to_string(), s["note_tone"].as_str().unwrap_or("").to_string())
        };
        let note = if note.is_empty() && n == 0 {
            "No ops yet. Use ADD TO JOB on the CONV page, or ADD STEP.".to_string()
        } else {
            note
        };
        live.set_job_note(note.into());
        live.set_job_note_tone(tone.into());
    }

    fn open_pick(&mut self, title: &str, items: Vec<String>, pick: Pick) {
        if let Some(ui) = self.ui.upgrade() {
            crate::picker::open(&ui, "job", title, items, None);
        }
        self.pick = Some(pick);
    }
}

/// Status: follow the controller (run state, op progress).
pub fn apply_status(j: &Rc<RefCell<Job>>, s: &Status) {
    j.borrow_mut().status = Some(s.clone());
}

/// Is `path` the folder job builds go in (or a program in it)?
pub fn folder(cfg: &Config, sim: bool) -> PathBuf {
    if sim {
        cache_dir().join("sim-jobs")
    } else {
        crate::files::root(cfg).join("jobs")
    }
}

pub fn cache_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(".cache/millgui")
}

pub fn wire(ui: &MainWindow, machine: &Handle, cfg: &Config, sim: bool) -> (Rc<RefCell<Job>>, slint::Timer) {
    let num = |s, k| cfg.get_f64(s, k).map_or(Value::Null, |v| json!(v));
    let init = json!({
        "cmd": "init",
        "folder": folder(cfg, sim),
        "nc_folder": if sim { cache_dir().join("sim-nc") } else { crate::files::root(cfg) },
        "tool_table": crate::tools::table_path(cfg, sim),
        "probe_tool": cfg.get_f64("PROBE", "TOOL_NUMBER").unwrap_or(99.0) as i64,
        "max_forward_rpm": num("SPINDLE_0", "MAX_FORWARD_VELOCITY"),
        "max_reverse_rpm": num("SPINDLE_0", "MAX_REVERSE_VELOCITY"),
    });
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
        pick: None,
        pins,
        machine: machine.clone(),
        ui: ui.as_weak(),
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

    let actions = ui.global::<Actions>();
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
        t.borrow_mut().open_pick("ADD A MANUAL STEP", STEP_PRESETS.iter().map(|s| s.to_string()).collect(), Pick::Step);
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
            Some(Pick::Step) => jb.act(json!({"cmd": "add_step", "index": i})),
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
        if jb.call(req).is_ok() && jb.snap != before {
            jb.refresh();
        }
    });
    (j, timer)
}
