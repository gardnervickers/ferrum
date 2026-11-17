//! The JOB page's state (`mymill_vcp/job_session.py`): the open job, its
//! build, and the HAL pins the built program reads.

use crate::compiler::{self, compile_job, dependency_problems, op_at_line, stale_reasons, stamp_line, verify, CompiledJob, Machine};
use crate::mill_ops::Generator;
use crate::model::{Job, Op, MAX_OPS};
use crate::py::{self, Error, Result};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const LAST_JOB_FILE: &str = ".last-job";

/// Preset messages for manual steps.
pub const STEP_PRESETS: [&str; 5] = [
    "FLIP PART AND RE-CLAMP",
    "CLEAR CHIPS AND CHECK THE PART",
    "MEASURE THE PART",
    "CHANGE VISE JAWS",
    "MOVE CLAMPS CLEAR OF THE NEXT OP",
];

/// Where the machine description comes from; read again on every use so
/// tool table edits are always seen.
pub type MachineFn<'a> = dyn Fn() -> Result<Machine> + 'a;

/// Ids for new ops and names for new jobs.
pub struct Hooks {
    pub new_id: Box<dyn FnMut() -> String + Send>,
    pub job_name: Box<dyn FnMut() -> String + Send>,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks { new_id: Box::new(random_id), job_name: Box::new(default_job_name) }
    }
}

/// `secrets.token_hex(4)`.
pub fn random_id() -> String {
    use std::io::Read;
    let mut buf = [0u8; 4];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_ok();
    if !ok {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let h = py::sha256_hex(format!("{}-{}-{}", t, std::process::id(), n).as_bytes());
        return h[..8].to_string();
    }
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}

/// `datetime.now().strftime('JOB %m-%d %H%M')` in local time.
pub fn default_job_name() -> String {
    // SAFETY: localtime_r writes only into the tm we pass.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("JOB {:02}-{:02} {:02}{:02}", tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
    }
}

/// The `mymill-job` pins (`FakePins`).
#[derive(Clone, Debug, PartialEq)]
pub struct Pins {
    pub stamp: i64,
    /// `skip[n - 1]` is `skip-n`.
    pub skip: [bool; MAX_OPS],
    pub stop_after: bool,
}

impl Default for Pins {
    fn default() -> Self {
        Pins { stamp: 0, skip: [false; MAX_OPS], stop_after: false }
    }
}

/// Where the built program is, for the op list to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct RunState {
    pub running: bool,
    pub paused: bool,
    pub position: Option<usize>,
    pub op_fraction: f64,
    pub job_fraction: f64,
    pub line: Option<i64>,
    pub first_line: Option<i64>,
    pub last_line: Option<i64>,
}

pub const IDLE: RunState = RunState {
    running: false,
    paused: false,
    position: None,
    op_fraction: 0.0,
    job_fraction: 0.0,
    line: None,
    first_line: None,
    last_line: None,
};

/// The controller status fields the session follows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stat {
    pub file: Option<String>,
    /// `interp_state == INTERP_IDLE`.
    pub idle: bool,
    pub motion_line: i64,
    pub current_line: i64,
    pub paused: bool,
}

type ShapeItem = (String, String, String, Value, Vec<String>, Vec<String>);

/// `os.path.join(a, b)`.
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{}{}", a, b)
    } else {
        format!("{}/{}", a, b)
    }
}

/// A Python path string resolved against the config dir (Python's cwd).
pub fn resolve(dir: &Path, p: &str) -> PathBuf {
    if p.starts_with('/') {
        PathBuf::from(p)
    } else {
        dir.join(p)
    }
}

/// `os.path.realpath`.
pub fn realpath(dir: &Path, p: &str) -> PathBuf {
    let full = resolve(dir, p);
    let mut normal = PathBuf::new();
    for c in full.components() {
        match c {
            std::path::Component::ParentDir => {
                normal.pop();
            }
            std::path::Component::CurDir => {}
            other => normal.push(other),
        }
    }
    if let Ok(c) = std::fs::canonicalize(&normal) {
        return c;
    }
    let mut tail = Vec::new();
    let mut head = normal.clone();
    while let Some(name) = head.file_name().map(|n| n.to_os_string()) {
        tail.push(name);
        head.pop();
        if let Ok(c) = std::fs::canonicalize(&head) {
            let mut out = c;
            for n in tail.iter().rev() {
                out.push(n);
            }
            return out;
        }
    }
    normal
}

pub fn same_file(dir: &Path, a: &str, b: &str) -> bool {
    realpath(dir, a) == realpath(dir, b)
}

fn mtime(path: &Path, shown: &str) -> Result<f64> {
    let meta = std::fs::metadata(path).map_err(|e| Error::io(&e, shown))?;
    let t = meta.modified().map_err(|e| Error::io(&e, shown))?;
    Ok(match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    })
}

fn makedirs(dir: &Path, shown: &str) -> Result<()> {
    let path = resolve(dir, shown);
    if path.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(&path).map_err(|e| Error::io(&e, shown))
}

fn write_text(path: &Path, shown: &str, text: &str) -> Result<()> {
    std::fs::write(path, text).map_err(|e| Error::io(&e, shown))
}

fn write_atomic(dir: &Path, path: &str, text: &str) -> Result<()> {
    let tmp = format!("{}.tmp", path);
    write_text(&resolve(dir, &tmp), &tmp, text)?;
    std::fs::rename(resolve(dir, &tmp), resolve(dir, path)).map_err(|e| Error::io(&e, &tmp))
}

/// `_safe_file_name`.
pub fn safe_file_name(name: &str) -> String {
    let mut out = String::new();
    let mut in_bad = false;
    for c in name.chars() {
        let good = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ' ' | '-');
        if good {
            out.push(c);
            in_bad = false;
        } else if !in_bad {
            out.push('_');
            in_bad = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == ' ' || c == '.');
    let name = if trimmed.is_empty() { "job" } else { trimmed };
    name.replace(' ', "_")
}

/// The open job, autosaved, and the build that was last loaded.
pub struct JobSession {
    pub dir: PathBuf,
    pub folder: String,
    pub pins: Pins,
    pub job: Job,
    pub path: Option<String>,
    pub build: Option<CompiledJob>,
    pub build_path: Option<String>,
    built_shape: Option<Vec<ShapeItem>>,
    pub stop_requested: bool,
    pub run: RunState,
    program_checks: HashMap<(String, u64), bool>,
}

impl JobSession {
    pub fn new(dir: PathBuf, folder: String, job_name: String, machine: &MachineFn) -> JobSession {
        let mut s = JobSession {
            dir,
            folder,
            pins: Pins::default(),
            job: Job { name: job_name, ops: vec![] },
            path: None,
            build: None,
            build_path: None,
            built_shape: None,
            stop_requested: false,
            run: IDLE,
            program_checks: HashMap::new(),
        };
        s.publish(machine);
        s
    }

    fn changed(&mut self, machine: &MachineFn) {
        self.publish(machine);
    }

    fn abs(&self, p: &str) -> PathBuf {
        resolve(&self.dir, p)
    }

    // -- files ------------------------------------------------------------------

    pub fn new_job(&mut self, name: String, machine: &MachineFn) -> Result<()> {
        self.require_editable()?;
        self.job = Job { name, ops: vec![] };
        self.path = Some(self.free_path(&self.job.name.clone(), ".job")?);
        self.save()?;
        self.changed(machine);
        Ok(())
    }

    pub fn open(&mut self, path: &str, machine: &MachineFn) -> Result<()> {
        self.require_editable()?;
        let text = py::read_text(&self.abs(path), path)?;
        self.job = Job::loads(&text)?;
        self.path = Some(path.to_string());
        self.remember(path);
        self.changed(machine);
        Ok(())
    }

    /// Reopen the job that was open when the screen last closed.
    pub fn restore_last(&mut self, machine: &MachineFn) -> bool {
        let last = join(&self.folder, LAST_JOB_FILE);
        let Ok(text) = py::read_text(&self.abs(&last), &last) else { return false };
        let path = py::strip(&text).to_string();
        if !path.is_empty() && self.abs(&path).is_file() {
            return match self.open(&path, machine) {
                Ok(()) => true,
                Err(Error::Job(_)) => false,
                Err(e) if e.is_os_error() || matches!(&e, Error::Py(k, _) if k == "UnicodeDecodeError") => false,
                Err(_) => false,
            };
        }
        false
    }

    pub fn job_files(&self) -> Result<Vec<String>> {
        let Ok(entries) = std::fs::read_dir(self.abs(&self.folder)) else { return Ok(vec![]) };
        let mut paths = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".job") {
                paths.push(join(&self.folder, &name));
            }
        }
        let mut keyed = Vec::new();
        for p in paths {
            let t = mtime(&self.abs(&p), &p)?;
            keyed.push((t, p));
        }
        keyed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        Ok(keyed.into_iter().map(|(_, p)| p).collect())
    }

    pub fn save(&mut self) -> Result<()> {
        if self.path.is_none() {
            self.path = Some(self.free_path(&self.job.name.clone(), ".job")?);
        }
        makedirs(&self.dir, &self.folder)?;
        let path = self.path.clone().unwrap();
        write_text(&self.abs(&path), &path, &self.job.dumps())?;
        self.remember(&path);
        Ok(())
    }

    fn remember(&self, path: &str) {
        if makedirs(&self.dir, &self.folder).is_ok() {
            let _ = write_atomic(&self.dir, &join(&self.folder, LAST_JOB_FILE), &format!("{}\n", path));
        }
    }

    fn free_path(&self, name: &str, ext: &str) -> Result<String> {
        makedirs(&self.dir, &self.folder)?;
        let base = join(&self.folder, &safe_file_name(name));
        let mut path = format!("{}{}", base, ext);
        let mut n = 1;
        while self.abs(&path).exists() {
            path = format!("{}_{}{}", base, n, ext);
            n += 1;
        }
        Ok(path)
    }

    // -- editing ------------------------------------------------------------------

    /// The op list cannot change shape while the built program runs.
    pub fn locked(&self) -> bool {
        self.run.running
    }

    fn require_editable(&self) -> Result<()> {
        if self.locked() {
            return Err(Error::job("The job is running. Stop it before changing the op list."));
        }
        Ok(())
    }

    /// Check one op on its own, then append it. Returns its position.
    pub fn add(&mut self, op: Op, machine: &MachineFn) -> Result<usize> {
        self.require_editable()?;
        if self.job.ops.len() >= MAX_OPS {
            return Err(Error::job(format!("A job can hold at most {} ops.", MAX_OPS)));
        }
        let check = Job { name: "check".into(), ops: vec![op.clone()] };
        compile_job(&check, &machine()?)?;
        self.job.ops.push(op);
        self.save()?;
        self.changed(machine);
        Ok(self.job.ops.len())
    }

    /// Add the ops of a CAM program, all or none: the job only changes when
    /// every op passes the checks together.
    pub fn add_cam(&mut self, source: &str, ops: Vec<crate::fusion::CamOp>, hooks: &mut Hooks, machine: &MachineFn) -> Result<usize> {
        self.require_editable()?;
        if self.job.ops.len() + ops.len() > MAX_OPS {
            return Err(Error::job(format!(
                "A job can hold at most {} ops; this program has {} and the job already has {}.",
                MAX_OPS,
                ops.len(),
                self.job.ops.len()
            )));
        }
        let new: Vec<Op> = ops
            .into_iter()
            .map(|c| {
                let mut params = Map::new();
                params.insert("source".into(), Value::String(source.into()));
                params.insert("setup".into(), c.setup.to_dict());
                params.insert("body".into(), Value::Array(c.body.into_iter().map(Value::String).collect()));
                Op::new((hooks.new_id)(), c.name, "cam", params)
            })
            .collect();
        let check = Job { name: "check".into(), ops: new.clone() };
        compile_job(&check, &machine()?)?;
        let n = new.len();
        self.job.ops.extend(new);
        self.save()?;
        self.changed(machine);
        Ok(n)
    }

    /// Add a conversational op from a configured generator.
    pub fn add_from_generator(
        &mut self,
        gen: &Generator,
        cycle: Option<&Value>,
        cycle_param: Option<&Value>,
        hooks: &mut Hooks,
        machine: &MachineFn,
    ) -> Result<usize> {
        let params = compiler::params_from_generator(gen, cycle, cycle_param)?;
        let name = compiler::default_name(&params);
        let op = Op::new((hooks.new_id)(), name, "conversational", params);
        self.add(op, machine)
    }

    /// Add a probing op made on the PROBE screen (`probe` module params).
    pub fn add_probe(&mut self, name: &str, params: Map<String, Value>, hooks: &mut Hooks, machine: &MachineFn) -> Result<usize> {
        let mut params = params;
        if params.get("mode").and_then(|v| v.as_str()) == Some("measure") && !params.contains_key("measure") {
            let routine = params.get("routine").and_then(|v| v.as_str()).unwrap_or("").to_string();
            params.insert("measure".into(), crate::probe::default_measure(&routine));
        }
        let mut op = Op::new((hooks.new_id)(), name.to_string(), "probe", params);
        op.provides = crate::probe::provides(&op);
        self.add(op, machine)
    }

    /// Change a MEASURE op's nominal, tolerance, value, checked op or
    /// on-fail action.
    pub fn set_measure(&mut self, op_id: &Value, changes: &Map<String, Value>, machine: &MachineFn) -> Result<()> {
        self.require_editable()?;
        let op = self.job.op_mut(op_id)?;
        crate::probe::set_measure(op, changes)?;
        self.save()?;
        self.changed(machine);
        Ok(())
    }

    pub fn add_step(&mut self, message: &Value, hooks: &mut Hooks, machine: &MachineFn) -> Result<usize> {
        let id = (hooks.new_id)();
        match message {
            Value::String(m) => {
                let mut params = Map::new();
                params.insert("message".into(), message.clone());
                self.add(Op::new(id, m.clone(), "step", params), machine)
            }
            other => {
                // A name that is not text never passes the job's checks.
                self.require_editable()?;
                if self.job.ops.len() >= MAX_OPS {
                    return Err(Error::job(format!("A job can hold at most {} ops.", MAX_OPS)));
                }
                machine()?;
                Err(Error::job(format!("Op \"{}\": name must not be empty", py::str_of(other))))
            }
        }
    }

    pub fn remove(&mut self, op_id: &Value, machine: &MachineFn) -> Result<()> {
        self.require_editable()?;
        let i = self.job.index(op_id)?;
        self.job.ops.remove(i);
        self.save()?;
        self.changed(machine);
        Ok(())
    }

    pub fn move_op(&mut self, op_id: &Value, offset: i64, machine: &MachineFn) -> Result<()> {
        self.require_editable()?;
        self.job.move_op(op_id, offset)?;
        self.save()?;
        self.changed(machine);
        Ok(())
    }

    /// An op can be toggled until the program reaches it.
    pub fn can_toggle(&self, op_id: &Value) -> bool {
        if !self.run.running || self.run.position.is_none() {
            return true;
        }
        match op_id.as_str().and_then(|id| self.build_position(id)) {
            Some(p) => p > self.run.position.unwrap(),
            None => false,
        }
    }

    pub fn set_enabled(&mut self, op_id: &Value, enabled: bool, machine: &MachineFn) -> Result<()> {
        if !self.can_toggle(op_id) {
            return Err(Error::job("That op has already started."));
        }
        self.job.op_mut(op_id)?.enabled = enabled;
        self.save()?;
        self.changed(machine);
        Ok(())
    }

    /// Skip every op before this one and run this one.
    pub fn start_at(&mut self, op_id: &Value, machine: &MachineFn) -> Result<()> {
        self.require_editable()?;
        let index = self.job.index(op_id)?;
        for (i, op) in self.job.ops.iter_mut().enumerate() {
            if i < index {
                op.enabled = false;
            } else if i == index {
                op.enabled = true;
            }
        }
        self.save()?;
        self.changed(machine);
        Ok(())
    }

    fn enabled_map(&self) -> HashMap<String, bool> {
        self.job.ops.iter().map(|o| (o.id.clone(), o.enabled)).collect()
    }

    pub fn warnings(&self) -> Vec<String> {
        dependency_problems(&self.job, &self.enabled_map())
    }

    // -- building -------------------------------------------------------------------

    /// Compile, write a new program file and remember it as the build.
    /// Returns the build and the path to load.
    pub fn build_and_load(&mut self, machine: &MachineFn) -> Result<(CompiledJob, String)> {
        self.require_editable()?;
        if self.job.ops.is_empty() {
            return Err(Error::job("Add an op before building."));
        }
        let m = machine()?;
        let compiled = compile_job(&self.job, &m)?;
        self.save()?;
        let base = safe_file_name(&self.job.name);
        let path = join(&self.folder, &format!("{}-{}.ngc", base, &compiled.stamp[..6]));
        write_atomic(&self.dir, &path, &compiled.text)?;
        self.remove_old_builds(&base, &path)?;
        self.build = Some(compiled.clone());
        self.build_path = Some(path.clone());
        self.built_shape = Some(self.shape());
        self.program_checks.clear();
        self.changed(machine);
        Ok((compiled, path))
    }

    fn remove_old_builds(&self, base: &str, keep: &str) -> Result<()> {
        let folder = self.abs(&self.folder);
        let entries = std::fs::read_dir(&folder).map_err(|e| Error::io(&e, &self.folder))?;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix(base).and_then(|r| r.strip_prefix('-')) else { continue };
            let Some(hex) = rest.strip_suffix(".ngc") else { continue };
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                continue;
            }
            let path = join(&self.folder, &name);
            if same_file(&self.dir, &path, keep) {
                continue;
            }
            let Ok(text) = py::read_text(&self.abs(&path), &path) else { continue };
            let first = text.split_inclusive('\n').next().unwrap_or("");
            if stamp_line(py::strip(first)).is_none() {
                continue;
            }
            let _ = std::fs::remove_file(self.abs(&path));
        }
        Ok(())
    }

    fn shape(&self) -> Vec<ShapeItem> {
        self.job
            .ops
            .iter()
            .map(|op| {
                (op.id.clone(), op.name.clone(), op.kind.clone(), Value::Object(op.params.clone()),
                 op.needs.clone(), op.provides.clone())
            })
            .collect()
    }

    fn shape_matches(&self) -> bool {
        let Some(built) = &self.built_shape else { return false };
        let now = self.shape();
        now.len() == built.len()
            && now.iter().zip(built).all(|(a, b)| {
                a.0 == b.0 && a.1 == b.1 && a.2 == b.2 && py::eq(&a.3, &b.3) && a.4 == b.4 && a.5 == b.5
            })
    }

    /// `(true, "")` when the loaded build may run, else `(false, why)`.
    pub fn readiness(&self, machine: &MachineFn) -> (bool, String) {
        let Some(build) = &self.build else {
            return (false, "Not built. Hold BUILD AND LOAD.".into());
        };
        if !self.shape_matches() {
            return (false, "The job changed since it was built. Hold BUILD AND LOAD.".into());
        }
        let m = match machine() {
            Ok(m) => m,
            Err(e) => {
                let text = match e {
                    Error::Job(errs) => errs.join("\n"),
                    Error::Py(_, msg) => msg,
                };
                return (false, format!("The tool table could not be read: {}", text));
            }
        };
        let reasons = stale_reasons(&build.depends, &m);
        if !reasons.is_empty() {
            return (false, format!("{} Hold BUILD AND LOAD.", reasons.join(" ")));
        }
        (true, String::new())
    }

    pub fn build_position(&self, op_id: &str) -> Option<usize> {
        self.build.as_ref()?.ranges.iter().find(|r| r.op_id == op_id).map(|r| r.position)
    }

    pub fn publish(&mut self, machine: &MachineFn) -> bool {
        let (ready, _) = self.readiness(machine);
        self.pins.stamp = if ready { self.build.as_ref().unwrap().stamp_pin_value() } else { 0 };
        let enabled = self.enabled_map();
        let by_position: HashMap<usize, String> = if ready {
            self.build.as_ref().unwrap().ranges.iter().map(|r| (r.position, r.op_id.clone())).collect()
        } else {
            HashMap::new()
        };
        for position in 1..=MAX_OPS {
            self.pins.skip[position - 1] = match by_position.get(&position) {
                Some(id) => !*enabled.get(id).unwrap_or(&true),
                None => false,
            };
        }
        self.pins.stop_after = self.stop_requested && ready;
        ready
    }

    // -- running ----------------------------------------------------------------------

    pub fn request_stop_after(&mut self, requested: bool, machine: &MachineFn) {
        self.stop_requested = requested;
        self.changed(machine);
    }

    pub fn is_build_loaded(&self, stat_file: Option<&str>) -> bool {
        match (stat_file, &self.build_path) {
            (Some(f), Some(b)) if !f.is_empty() && !b.is_empty() => same_file(&self.dir, f, b),
            _ => false,
        }
    }

    /// Follow the controller.
    pub fn poll(&mut self, stat: &Stat, machine: &MachineFn) -> RunState {
        let before = (self.run.running, self.run.position, self.stop_requested);
        let running = self.is_build_loaded(stat.file.as_deref()) && !stat.idle;
        if !running {
            self.run = IDLE;
            if self.stop_requested {
                self.stop_requested = false;
            }
        } else {
            self.run = self.run_state(stat);
            let build = self.build.as_ref().unwrap();
            let stop_lines: Vec<i64> = build.ranges.iter().filter_map(|r| r.stop_line).filter(|l| *l != 0).collect();
            if self.stop_requested && (stop_lines.contains(&stat.motion_line) || stop_lines.contains(&stat.current_line)) {
                self.stop_requested = false;
            }
        }
        let after = (self.run.running, self.run.position, self.stop_requested);
        if after != before {
            self.changed(machine);
        }
        self.run.clone()
    }

    fn run_state(&self, stat: &Stat) -> RunState {
        let build = self.build.as_ref().unwrap();
        let line = if stat.motion_line != 0 { stat.motion_line } else { stat.current_line };
        let current = if line != 0 { op_at_line(&build.ranges, line) } else { None };
        let enabled = self.enabled_map();
        let weights: HashMap<usize, i64> = build
            .ranges
            .iter()
            .map(|r| {
                let w = if *enabled.get(&r.op_id).unwrap_or(&true) { r.last_line - r.first_line + 1 } else { 0 };
                (r.position, w)
            })
            .collect();
        let sum: i64 = weights.values().sum();
        let total = if sum == 0 { 1.0 } else { sum as f64 };
        let Some(current) = current else {
            let last = build.ranges.last().map(|r| r.last_line).unwrap_or(0);
            let done = if line != 0 && line > last { sum } else { 0 };
            return RunState {
                running: true,
                paused: stat.paused,
                line: Some(line),
                job_fraction: done as f64 / total,
                ..IDLE
            };
        };
        let span = 1.max(current.last_line - current.first_line);
        let raw = (line - current.first_line) as f64 / span as f64;
        let op_fraction = { let v = if raw > 0.0 { raw } else { 0.0 }; if v < 1.0 { v } else { 1.0 } };
        let before: i64 = weights.iter().filter(|(p, _)| **p < current.position).map(|(_, w)| *w).sum();
        let done = before as f64 + weights[&current.position] as f64 * op_fraction;
        RunState {
            running: true,
            paused: stat.paused,
            position: Some(current.position),
            op_fraction,
            job_fraction: done / total,
            line: Some(line),
            first_line: Some(current.first_line),
            last_line: Some(current.last_line),
        }
    }

    /// Why cycle start must not start the loaded program, or `None`.
    pub fn cycle_start_problem(&mut self, stat: &Stat, line_number: i64, machine: &MachineFn) -> Result<Option<String>> {
        let path = match &stat.file {
            Some(p) if !p.is_empty() => p.clone(),
            _ => return Ok(None),
        };
        if !self.is_job_program(&path)? {
            return Ok(None);
        }
        if !stat.idle {
            return Ok(None);
        }
        if line_number != 0 {
            return Ok(Some(
                "Run from line cannot start inside a job: it would skip the tool change and setup. \
                 Use START AT OP on the JOB page."
                    .into(),
            ));
        }
        if !self.is_build_loaded(Some(&path)) {
            return Ok(Some(
                "This job program was not built by the JOB page in this session. \
                 Open its job on the JOB page and hold BUILD AND LOAD."
                    .into(),
            ));
        }
        let text = match py::read_text(&self.abs(&path), &path) {
            Ok(t) => t,
            Err(e) if e.is_os_error() => {
                let msg = match e {
                    Error::Py(_, m) => m,
                    other => other.to_string(),
                };
                return Ok(Some(format!("The job program could not be read: {}", msg)));
            }
            Err(e) => return Err(e),
        };
        let build = self.build.as_ref().unwrap();
        if text != build.text || !verify(&text, &build.depends) {
            return Ok(Some("The job program was changed after it was built. Hold BUILD AND LOAD.".into()));
        }
        let (ready, why) = self.readiness(machine);
        Ok(if ready { None } else { Some(why) })
    }

    fn is_job_program(&mut self, path: &str) -> Result<bool> {
        let Ok(t) = mtime(&self.abs(path), path) else { return Ok(false) };
        let key = (path.to_string(), t.to_bits());
        if let Some(v) = self.program_checks.get(&key) {
            return Ok(*v);
        }
        let first = match py::read_text(&self.abs(path), path) {
            Ok(text) => py::strip(text.split_inclusive('\n').next().unwrap_or("")).to_string(),
            Err(e) if e.is_os_error() => String::new(),
            Err(e) => return Err(e),
        };
        let v = stamp_line(&first).is_some();
        self.program_checks.insert(key, v);
        Ok(v)
    }
}
