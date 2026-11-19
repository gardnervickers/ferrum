//! millgui's job requests (`millgui/job_helper.py`): one JSON request in,
//! one JSON reply out, exactly as the Python helper's `main()` wrote them.

use crate::compiler::{read_tool_table, Machine};
use crate::mill_ops::{self, GenError, Generator, Kind};
use crate::model::MAX_OPS;
use crate::py::{self, Error, Result};
use crate::session::{self, Hooks, JobSession, Stat, STEP_PRESETS};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const NO_SNAP: [&str; 5] = ["cycle_start", "conv", "thread", "files", "presets"];

/// `_fmt`: three places, trailing zeros removed.
fn fmt3(value: f64) -> String {
    let t = py::fixed(value, 3);
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t.is_empty() { "0".into() } else { t.into() }
}

/// The generators' summaries for people: 100.0000 -> 100, 2.5000 -> 2.5,
/// "1 depth pass(es)" -> "1 depth pass", "2 hole(s)" -> "2 holes".
pub fn tidy(text: &str) -> String {
    static RE: OnceLock<[Regex; 4]> = OnceLock::new();
    let re = RE.get_or_init(|| {
        [
            Regex::new(r"(\d+\.\d*?[1-9])0+\b").unwrap(),
            Regex::new(r"(\d+)\.0+\b").unwrap(),
            Regex::new(r"\b1 ((?:\w+ )*?\w+)\((?:es|s)\)").unwrap(),
            Regex::new(r"(\w+)\((es|s)\)").unwrap(),
        ]
    });
    let t = re[0].replace_all(text, "${1}");
    let t = re[1].replace_all(&t, "${1}");
    let t = re[2].replace_all(&t, "1 ${1}");
    re[3].replace_all(&t, "${1}${2}").into_owned()
}

/// Tool comments from the tool table, by tool number.
fn tool_comments(dir: &Path, path: &str) -> BTreeMap<i64, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = py::read_text(&session::resolve(dir, path), path) else { return out };
    for line in py::file_lines(&text) {
        let line = py::strip(line);
        if !line.to_uppercase().starts_with('T') || !line.contains(';') {
            continue;
        }
        let (head, comment) = line.split_once(';').unwrap();
        let Some(first) = py::split(head).first().copied() else { continue };
        let digits: String = first.chars().skip(1).collect();
        if let Ok(n) = py::parse_int(&digits) {
            out.insert(n, py::strip(comment).to_string());
        }
    }
    out
}

fn get<'a>(req: &'a Map<String, Value>, key: &str) -> Result<&'a Value> {
    req.get(key).ok_or_else(|| Error::key_str(key))
}

fn path_arg(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        other => Err(Error::type_error(format!(
            "expected str, bytes or os.PathLike object, not {}",
            py::type_name(other)
        ))),
    }
}

/// `os.path.basename`.
fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// `os.path.splitext(name)[0]` for a base name.
fn strip_ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if name[..i].chars().any(|c| c != '.') => &name[..i],
        _ => name,
    }
}

/// The JOB page and conversational pages' backend.
pub struct Helper {
    config_dir: PathBuf,
    cfg: Value,
    session: Option<JobSession>,
    hooks: Hooks,
}

impl Helper {
    /// `config_dir` plays the part of the Python helper's working directory:
    /// relative paths in requests are resolved against it.
    pub fn new(config_dir: PathBuf) -> Helper {
        Helper::with_hooks(config_dir, Hooks::default())
    }

    /// As `new`, with the op id and default job name sources replaced
    /// (tests use counters and fixed names).
    pub fn with_hooks(config_dir: PathBuf, hooks: Hooks) -> Helper {
        Helper { config_dir, cfg: Value::Object(Map::new()), session: None, hooks }
    }

    pub fn session(&self) -> Option<&JobSession> {
        self.session.as_ref()
    }

    /// Handle one request and return the reply `main()` would write.
    pub fn handle(&mut self, req: &Value) -> Value {
        let mut reply = match self.dispatch(req) {
            Ok(mut out) => {
                out.insert("ok".into(), Value::Bool(true));
                out
            }
            Err(Error::Job(errors)) => {
                let error = if errors.len() == 1 {
                    errors[0].clone()
                } else {
                    format!("{} (+{} more)", errors[0], errors.len() - 1)
                };
                let mut m = Map::new();
                m.insert("ok".into(), Value::Bool(false));
                m.insert("error".into(), Value::String(error));
                m.insert("errors".into(), json!(errors));
                m
            }
            Err(Error::Py(kind, msg)) => {
                let mut m = Map::new();
                m.insert("ok".into(), Value::Bool(false));
                m.insert("error".into(), Value::String(format!("{}: {}", kind, msg)));
                m
            }
        };
        if self.session.is_some() && !reply.contains_key("snap") && reply.get("ok") == Some(&Value::Bool(false)) {
            if let Ok(snap) = self.snapshot() {
                reply.insert("snap".into(), snap);
            }
        }
        Value::Object(reply)
    }

    /// Handle one request line: `json.loads` it, then `handle`. Returns
    /// `None` for a blank line, which `main()` skips.
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        let line = py::strip(line);
        if line.is_empty() {
            return None;
        }
        Some(match py::loads(line) {
            Ok(req) => self.handle(&req),
            Err(e) => {
                let mut m = Map::new();
                m.insert("ok".into(), Value::Bool(false));
                m.insert("error".into(), Value::String(format!("JSONDecodeError: {}", e)));
                if self.session.is_some() {
                    if let Ok(snap) = self.snapshot() {
                        m.insert("snap".into(), snap);
                    }
                }
                Value::Object(m)
            }
        })
    }

    fn dispatch(&mut self, req: &Value) -> Result<Map<String, Value>> {
        let r = match req {
            Value::Object(m) => m,
            other => return Err(Error::attribute(py::type_name(other), "get")),
        };
        let cmd = r.get("cmd").cloned().unwrap_or(Value::Null);
        let name = cmd.as_str().unwrap_or("");
        let mut out = match name {
            "init" => self.cmd_init(req, r)?,
            "poll" => self.cmd_poll(r)?,
            "cycle_start" => self.cmd_cycle_start(r)?,
            "files" => self.cmd_files()?,
            "new" => self.cmd_new()?,
            "open" => self.cmd_open(r)?,
            "add_step" => self.cmd_add_step(r)?,
            "import_cam" => self.cmd_import_cam(r)?,
            "remove" => self.cmd_remove(r)?,
            "move" => self.cmd_move(r)?,
            "set_enabled" => self.cmd_set_enabled(r)?,
            "start_at" => self.cmd_start_at(r)?,
            "stop_after" => self.cmd_stop_after(r)?,
            "build" => self.cmd_build()?,
            "presets" => self.cmd_presets(),
            "thread" => self.cmd_thread(r)?,
            "conv" => self.cmd_conv(r)?,
            "add_probe" => self.cmd_add_probe(r)?,
            "add_tool_check" => self.cmd_add_tool_check(r)?,
            "add_tool_checks" => self.cmd_add_tool_checks()?,
            "set_measure" => self.cmd_set_measure(r)?,
            _ => return Err(Error::value_error(format!("unknown command {}", py::repr(&cmd)))),
        };
        if self.session.is_some() && !out.contains_key("snap") && !NO_SNAP.contains(&name) {
            out.insert("snap".into(), self.snapshot()?);
        }
        Ok(out)
    }

    // -- machine --------------------------------------------------------------------

    fn machine_from(cfg: &Value, dir: &Path) -> Result<Machine> {
        let c = cfg.as_object().cloned().unwrap_or_default();
        let table = path_arg(get(&c, "tool_table")?)?;
        let tools = read_tool_table(&session::resolve(dir, &table), &table)?;
        let probe = c.get("probe_tool").filter(|v| py::truthy(v)).cloned().unwrap_or(Value::Number(99.into()));
        let rpm = |key: &str| c.get(key).and_then(py::as_number);
        let mut m = Machine::new(tools, py::to_int(&probe)?, rpm("max_forward_rpm"), rpm("max_reverse_rpm"));
        m.units = match c.get("units").and_then(|u| u.as_str()).map(py::strip_upper).as_deref() {
            Some("MM") => Some("MM".into()),
            Some("IN" | "INCH") => Some("IN".into()),
            _ => None,
        };
        Ok(m)
    }

    fn machine(&self) -> Result<Machine> {
        Self::machine_from(&self.cfg, &self.config_dir)
    }

    /// The init request says the machine is in inches.
    fn inch(&self) -> bool {
        matches!(self.cfg.get("units").and_then(|u| u.as_str()).map(py::strip_upper).as_deref(), Some("IN" | "INCH"))
    }

    /// Run `f` on the session with the machine source; AttributeError like
    /// Python when there is no session yet.
    fn with_session<T>(
        &mut self,
        method: &str,
        f: impl FnOnce(&mut JobSession, &dyn Fn() -> Result<Machine>, &mut Hooks) -> Result<T>,
    ) -> Result<T> {
        let cfg = &self.cfg;
        let dir = &self.config_dir;
        let machine = move || Self::machine_from(cfg, dir);
        match self.session.as_mut() {
            Some(s) => f(s, &machine, &mut self.hooks),
            None => Err(Error::attribute("NoneType", method)),
        }
    }

    fn require_session(&self, method: &str) -> Result<()> {
        if self.session.is_none() {
            return Err(Error::attribute("NoneType", method));
        }
        Ok(())
    }

    // -- snapshot -------------------------------------------------------------------------

    fn detail(&self, op: &crate::model::Op, comments: &BTreeMap<i64, String>) -> Result<String> {
        match op.kind.as_str() {
            "step" => Ok("PAUSES AT MACHINE TOP  ·  SPINDLE AND COOLANT OFF".into()),
            "conversational" => {
                let p = &op.params;
                let tool = p.get("tool_number").cloned().unwrap_or(Value::Null);
                let comment = py::as_number(&tool)
                    .filter(|n| n.fract() == 0.0)
                    .and_then(|n| comments.get(&(n as i64)).cloned())
                    .unwrap_or_default();
                let rpm = py::to_float(p.get("spindle_rpm").unwrap_or(&Value::Number(0.into())))?;
                let parts = vec![
                    Value::String(format!("T{}", py::str_of(&tool))),
                    Value::String(comment),
                    Value::String(format!("{} RPM", fmt3(rpm))),
                    p.get("wcs").cloned().unwrap_or(Value::String(String::new())),
                ];
                Ok(parts.iter().filter(|x| py::truthy(x)).map(|x| py::str_of(x).to_uppercase()).collect::<Vec<_>>().join("  ·  "))
            }
            "cam" => {
                let setup = op.params.get("setup").cloned().unwrap_or(Value::Object(Map::new()));
                let Value::Object(setup) = setup else {
                    return Err(Error::attribute(py::type_name(&setup), "get"));
                };
                let tool = setup.get("tool").cloned().unwrap_or(Value::Null);
                let source = op.params.get("source").cloned().unwrap_or(Value::String(String::new()));
                Ok(format!("T{}  ·  FROM {}", py::str_of(&tool), py::str_of(&source).to_uppercase()))
            }
            "probe" => Ok(crate::probe::detail(op, self.inch())),
            _ => Ok(String::new()),
        }
    }

    /// The op list and status for the JOB page.
    pub fn snapshot(&self) -> Result<Value> {
        let s = self.session.as_ref().ok_or_else(|| Error::attribute("NoneType", "run"))?;
        let machine = || self.machine();
        let run = &s.run;
        let table = path_arg(get(self.cfg.as_object().unwrap_or(&Map::new()), "tool_table")?)?;
        let comments = tool_comments(&self.config_dir, &table);
        let mut ops = Vec::new();
        for (i, op) in s.job.ops.iter().enumerate() {
            let bp = s.build_position(&op.id);
            let state = match (run.running, run.position, bp) {
                (true, Some(rp), Some(bp)) => {
                    if !op.enabled {
                        "skipped"
                    } else if bp < rp {
                        "done"
                    } else if bp == rp {
                        "current"
                    } else {
                        "pending"
                    }
                }
                _ => if op.enabled { "idle" } else { "skipped" },
            };
            let badge = match op.kind.as_str() {
                "conversational" => "CONV".to_string(),
                "cam" => "CAM".to_string(),
                "step" => "STEP".to_string(),
                "probe" => "PROBE".to_string(),
                "toolcheck" => "TOOL".to_string(),
                other => other.to_uppercase(),
            };
            let detail = if op.kind == "toolcheck" { tool_check_detail(&s.job, i) } else { self.detail(op, &comments)? };
            let mut row = json!({
                "id": op.id, "pos": i + 1, "name": op.name.to_uppercase(), "badge": badge,
                "kind": op.kind, "enabled": op.enabled, "detail": detail,
                "state": state, "can_toggle": s.can_toggle(&Value::String(op.id.clone())),
            });
            if op.kind == "probe" {
                row["probe"] = probe_snap(op, &s.job, &self.machine().ok());
            }
            ops.push(row);
        }
        let (ready, why) = s.readiness(&machine);
        let (text, tone) = if run.running {
            let total = s.build.as_ref().map(|b| b.ranges.len()).unwrap_or(0);
            match run.position {
                Some(p) if p != 0 => (format!("RUNNING OP {} OF {}", p, total), "info"),
                _ => ("RUNNING".to_string(), "info"),
            }
        } else if ready {
            ("BUILT · LOADED".to_string(), "info")
        } else if s.build.is_none() {
            ("NOT BUILT".to_string(), "")
        } else {
            ("REBUILD NEEDED".to_string(), "warn")
        };
        let warnings = s.warnings();
        let (note, note_tone) = if !ready && s.build.is_some() && !run.running {
            (why, "warn")
        } else if let Some(w) = warnings.first() {
            (w.clone(), "warn")
        } else if s.stop_requested {
            ("THE JOB WILL STOP WITH THE SPINDLE OFF BEFORE THE NEXT OP.".to_string(), "")
        } else {
            (String::new(), "")
        };
        let progress = match (run.running, run.line, run.last_line) {
            (true, Some(line), Some(last)) if line != 0 && last != 0 => {
                let first = run.first_line.unwrap_or(0);
                format!("LINE {} OF {}", line - first + 1, last - first + 1)
            }
            _ => String::new(),
        };
        let skip: Vec<usize> = (1..=MAX_OPS).filter(|p| s.pins.skip[p - 1]).collect();
        Ok(json!({
            "name": s.job.name.to_uppercase(), "path": s.path.clone().unwrap_or_default(), "ops": ops,
            "state": text, "tone": tone, "ready": ready, "note": note, "note_tone": note_tone,
            "locked": s.locked(), "running": run.running, "position": run.position.unwrap_or(0),
            "op_fraction": run.op_fraction, "job_fraction": run.job_fraction, "progress": progress,
            "stop_requested": s.stop_requested,
            "pins": {"stamp": s.pins.stamp, "skip": skip, "stop_after": s.pins.stop_after},
        }))
    }

    // -- requests ---------------------------------------------------------------------------

    fn presets_value() -> Value {
        Value::Array(mill_ops::presets().iter().map(|p| Value::String(p.name.into())).collect())
    }

    fn cmd_init(&mut self, req: &Value, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.cfg = req.clone();
        let folder = path_arg(get(r, "folder")?)?;
        let target = session::resolve(&self.config_dir, &folder);
        if !target.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| Error::io(&e, &folder))?;
        }
        let name = (self.hooks.job_name)();
        let cfg = &self.cfg;
        let dir = &self.config_dir;
        let machine = || Self::machine_from(cfg, dir);
        let mut s = JobSession::new(self.config_dir.clone(), folder, name, &machine);
        s.restore_last(&machine);
        self.session = Some(s);
        let mut out = Map::new();
        out.insert("presets".into(), Self::presets_value());
        Ok(out)
    }

    fn stat(r: &Map<String, Value>) -> Result<Stat> {
        let st = match r.get("stat") {
            Some(v) if py::truthy(v) => v.clone(),
            _ => Value::Object(Map::new()),
        };
        let Value::Object(st) = st else {
            return Err(Error::attribute(py::type_name(&st), "get"));
        };
        let line = |key: &str| st.get(key).and_then(py::as_number).map(|f| f.trunc() as i64).unwrap_or(0);
        Ok(Stat {
            file: st.get("file").and_then(|v| v.as_str()).map(|s| s.to_string()).or_else(|| {
                if st.contains_key("file") { None } else { Some(String::new()) }
            }),
            idle: st.get("idle").map(py::truthy).unwrap_or(true),
            motion_line: line("motion_line"),
            current_line: line("current_line"),
            paused: st.get("paused").map(py::truthy).unwrap_or(false),
        })
    }

    fn cmd_poll(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.require_session("poll")?;
        let stat = Self::stat(r)?;
        let publish = r.get("publish").map(py::truthy).unwrap_or(false);
        self.with_session("poll", |s, m, _| {
            s.poll(&stat, m);
            if publish {
                s.publish(m);
            }
            Ok(())
        })?;
        Ok(Map::new())
    }

    fn cmd_cycle_start(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.require_session("cycle_start_problem")?;
        let stat = Self::stat(r)?;
        let line = match r.get("line") {
            Some(v) if py::truthy(v) => py::to_int(v)?,
            _ => 0,
        };
        let problem = self.with_session("cycle_start_problem", |s, m, _| s.cycle_start_problem(&stat, line, m))?;
        let mut out = Map::new();
        out.insert("problem".into(), problem.map(Value::String).unwrap_or(Value::Null));
        Ok(out)
    }

    fn cmd_files(&mut self) -> Result<Map<String, Value>> {
        let paths = self.with_session("job_files", |s, _, _| s.job_files())?;
        let names: Vec<String> =
            paths.iter().map(|p| strip_ext(basename(p)).replace('_', " ").to_uppercase()).collect();
        let mut out = Map::new();
        out.insert("paths".into(), json!(paths));
        out.insert("names".into(), json!(names));
        Ok(out)
    }

    fn cmd_new(&mut self) -> Result<Map<String, Value>> {
        self.with_session("new", |s, m, hooks| {
            let name = (hooks.job_name)();
            s.new_job(name, m)
        })?;
        Ok(Map::new())
    }

    fn cmd_open(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.require_session("open")?;
        let path = path_arg(get(r, "path")?)?;
        self.with_session("open", |s, m, _| s.open(&path, m))?;
        Ok(Map::new())
    }

    fn cmd_add_probe(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        let name = py::str_of(get(r, "name")?);
        let params = get(r, "params")?.as_object().cloned().ok_or_else(|| Error::job("params must be an object"))?;
        let pos = self.with_session("add_probe", |s, m, hooks| s.add_probe(&name, params, hooks, m))?;
        let mut out = Map::new();
        out.insert("message".into(), Value::String(format!("ADDED {} AS OP {}.", name.to_uppercase(), pos)));
        Ok(out)
    }

    fn cmd_add_tool_check(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        let after = r.get("after").filter(|v| !v.is_null()).cloned();
        let pos = self.with_session("add_tool_check", |s, m, hooks| s.add_tool_check(after.as_ref(), hooks, m))?;
        let mut out = Map::new();
        out.insert("message".into(), Value::String(format!("ADDED A TOOL CHECK AS OP {}.", pos)));
        Ok(out)
    }

    fn cmd_add_tool_checks(&mut self) -> Result<Map<String, Value>> {
        let n = self.with_session("add_tool_checks", |s, m, hooks| s.add_tool_checks(hooks, m))?;
        let mut out = Map::new();
        let text = match n {
            0 => "EVERY TOOL IS ALREADY CHECKED BEFORE IT IS PUT AWAY.".to_string(),
            1 => "ADDED 1 TOOL CHECK.".to_string(),
            n => format!("ADDED {} TOOL CHECKS.", n),
        };
        out.insert("message".into(), Value::String(text));
        Ok(out)
    }

    fn cmd_set_measure(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        let id = get(r, "id")?.clone();
        let changes = get(r, "changes")?.as_object().cloned().ok_or_else(|| Error::job("changes must be an object"))?;
        self.with_session("set_measure", |s, m, _| s.set_measure(&id, &changes, m))?;
        Ok(Map::new())
    }

    fn cmd_add_step(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        let message = match r.get("message") {
            Some(m) if py::truthy(m) => m.clone(),
            _ => {
                let index = py::to_int(r.get("index").unwrap_or(&Value::Number(0.into())))?;
                let n = STEP_PRESETS.len() as i64;
                let i = if index < 0 { index + n } else { index };
                if i < 0 || i >= n {
                    return Err(Error::py("IndexError", "tuple index out of range"));
                }
                Value::String(STEP_PRESETS[i as usize].into())
            }
        };
        let pos = self.with_session("add_step", |s, m, hooks| s.add_step(&message, hooks, m))?;
        let mut out = Map::new();
        out.insert("message".into(), Value::String(format!("ADDED STEP AS OP {}.", pos)));
        Ok(out)
    }

    /// Split a Fusion program into ops and add them. Any doubt refuses the
    /// whole program, with the reason; it can still run whole from FILES.
    fn cmd_import_cam(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.require_session("import_cam")?;
        let path = PathBuf::from(py::str_of(get(r, "path")?));
        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let refuse = |why: String| {
            Error::job(format!("{} was not added: {}. Run it whole from FILES instead.", file.to_uppercase(), why))
        };
        let text = std::fs::read_to_string(&path).map_err(|e| refuse(format!("it could not be read ({e})")))?;
        let ops = crate::fusion::split(&text).map_err(refuse)?;
        let n = self
            .with_session("import_cam", |s, m, hooks| s.add_cam(&file, ops, hooks, m))
            .map_err(|e| match e {
                Error::Job(errs) => refuse(errs.join("; ")),
                other => other,
            })?;
        let mut out = Map::new();
        out.insert(
            "message".into(),
            Value::String(format!("ADDED {} OP{} FROM {}.", n, if n == 1 { "" } else { "S" }, file.to_uppercase())),
        );
        Ok(out)
    }

    fn cmd_remove(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.require_session("remove")?;
        let id = get(r, "id")?.clone();
        self.with_session("remove", |s, m, _| s.remove(&id, m))?;
        Ok(Map::new())
    }

    fn cmd_move(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.require_session("move")?;
        let id = get(r, "id")?.clone();
        let offset = py::to_int(get(r, "offset")?)?;
        self.with_session("move", |s, m, _| s.move_op(&id, offset, m))?;
        Ok(Map::new())
    }

    fn cmd_set_enabled(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.require_session("set_enabled")?;
        let id = get(r, "id")?.clone();
        let enabled = py::truthy(get(r, "enabled")?);
        self.with_session("set_enabled", |s, m, _| s.set_enabled(&id, enabled, m))?;
        Ok(Map::new())
    }

    fn cmd_start_at(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        self.require_session("start_at")?;
        let id = get(r, "id")?.clone();
        self.with_session("start_at", |s, m, _| s.start_at(&id, m))?;
        Ok(Map::new())
    }

    fn cmd_stop_after(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        let on = r.get("on").map(py::truthy).unwrap_or(false);
        self.with_session("request_stop_after", |s, m, _| {
            s.request_stop_after(on, m);
            Ok(())
        })?;
        Ok(Map::new())
    }

    fn cmd_build(&mut self) -> Result<Map<String, Value>> {
        let (compiled, path) = self.with_session("build_and_load", |s, m, _| s.build_and_load(m))?;
        let mut out = Map::new();
        out.insert("load".into(), Value::String(path));
        if let Some(w) = compiled.warnings.first() {
            out.insert("message".into(), Value::String(w.clone()));
            out.insert("tone".into(), Value::String("warn".into()));
        } else {
            out.insert("message".into(), Value::String(match compiled.ranges.len() {
                1 => "BUILT 1 OP AND LOADED IT.".to_string(),
                n => format!("BUILT {n} OPS AND LOADED THEM."),
            }));
            out.insert("tone".into(), Value::String("ready".into()));
        }
        Ok(out)
    }

    fn cmd_presets(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("presets".into(), Self::presets_value());
        out
    }

    fn cmd_thread(&self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        let mut out = Map::new();
        let Some(preset) = mill_ops::find_preset(get(r, "name")?) else {
            out.insert("found".into(), Value::Bool(false));
            return Ok(out);
        };
        let units = match r.get("units") {
            None => "MM".to_string(),
            Some(Value::String(u)) => u.clone(),
            Some(other) => return Err(Error::attribute(py::type_name(other), "strip")),
        };
        let (major, pitch, drill) = mill_ops::preset_values(&preset, &units);
        out.insert("found".into(), Value::Bool(true));
        out.insert("major".into(), py::float_value(major));
        out.insert("pitch".into(), py::float_value(pitch));
        out.insert("drill".into(), py::float_value(drill));
        Ok(out)
    }

    fn generator(&self, r: &Map<String, Value>) -> Result<Generator> {
        let op_type = get(r, "type")?;
        let kind = op_type.as_str().and_then(Kind::from_type).ok_or_else(|| Error::key(op_type))?;
        let mut gen = Generator::new(kind);
        let attrs = match r.get("attrs") {
            Some(v) if py::truthy(v) => v.clone(),
            _ => Value::Object(Map::new()),
        };
        let Value::Object(attrs) = attrs else {
            return Err(Error::attribute(py::type_name(&attrs), "items"));
        };
        for (key, value) in &attrs {
            match key.as_str() {
                "tool_number" | "radial_passes" => {
                    gen.set_attr(key, &Value::Number(py::to_int(value)?.into()))?;
                }
                _ => gen.set_attr(key, value)?,
            }
        }
        if matches!(kind, Kind::HelicalBore | Kind::ThreadMill) {
            gen.z_feed = gen.xy_feed;
        }
        let machine = self.machine()?;
        let tool = if gen.tool_number.fract() == 0.0 { machine.tool(gen.tool_number as i64).copied() } else { None };
        gen.machine_units = machine.units.clone();
        if gen.has_attr("tool_diameter") {
            // The tool table is in machine units; the op may be in the other.
            gen.tool_diameter = tool.map(|t| machine.length_in(t.diameter, &gen.units)).unwrap_or(0.0);
        }
        gen.max_spindle_rpm = machine.max_rpm(&py::strip_upper(&gen.spindle_dir));
        Ok(gen)
    }

    fn cmd_conv(&mut self, r: &Map<String, Value>) -> Result<Map<String, Value>> {
        let gen = self.generator(r)?;
        let cycle = r.get("cycle").cloned().unwrap_or(Value::String("DRILL".into()));
        let cycle_text = py::str_of(&cycle);
        let is_drill = gen.kind == Kind::Drill;
        let param = if is_drill {
            match r.get("cycle_param") {
                Some(v) if py::truthy(v) => py::to_float(v)?,
                _ => 0.0,
            }
        } else {
            0.0
        };
        let mut out = Map::new();
        let lines = match gen.lines(&cycle_text, param) {
            Ok(lines) => lines,
            Err(GenError::Operation(errors)) => {
                out.insert("errors".into(), json!(errors));
                return Ok(out);
            }
            Err(GenError::Py(e)) => return Err(e),
        };
        let summary = tidy(&gen.summary(&cycle_text)?);
        let action = r.get("action").cloned().unwrap_or(Value::String("check".into()));
        out.insert("errors".into(), json!([]));
        out.insert("summary".into(), Value::String(summary));
        if gen.kind == Kind::Face {
            out.insert("gcode".into(), Value::String(lines.join("\n")));
        }
        if action == "add" {
            let cp = if is_drill { r.get("cycle_param").cloned() } else { None };
            let cyc = if is_drill { Some(cycle.clone()) } else { None };
            let pos = self.with_session("add_from_generator", |s, m, hooks| {
                s.add_from_generator(&gen, cyc.as_ref(), cp.as_ref(), hooks, m)
            })?;
            let name = self.session.as_ref().unwrap().job.name.clone();
            out.insert(
                "message".into(),
                Value::String(format!("Added to {} as op {}. Hold BUILD AND LOAD on the JOB page.", name, pos)),
            );
            out.insert("snap".into(), self.snapshot()?);
        } else if action == "post" {
            let name = match r.get("name") {
                Some(v) if py::truthy(v) => path_arg(v)?,
                _ => "conv".into(),
            };
            let path = self.free_program_path(&name)?;
            let text = mill_ops::program_text_to(&[lines], &gen.top());
            std::fs::write(session::resolve(&self.config_dir, &path), text).map_err(|e| Error::io(&e, &path))?;
            out.insert("message".into(), Value::String(format!("Saved {} and loaded it.", basename(&path))));
            out.insert("load".into(), Value::String(path));
        }
        Ok(out)
    }

    fn free_program_path(&self, name: &str) -> Result<String> {
        let c = self.cfg.as_object().cloned().unwrap_or_default();
        let folder = path_arg(get(&c, "nc_folder")?)?;
        let target = session::resolve(&self.config_dir, &folder);
        if !target.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| Error::io(&e, &folder))?;
        }
        let cleaned: String = basename(name)
            .chars()
            .map(|c| if c.is_alphanumeric() || " ._-".contains(c) { c } else { '_' })
            .collect();
        let cleaned = cleaned.trim_matches(|c| c == ' ' || c == '.');
        let mut name = if cleaned.is_empty() { "conv".to_string() } else { cleaned.to_string() }.replace(' ', "_");
        if name.to_lowercase().ends_with(".ngc") {
            let keep = name.chars().count() - 4;
            name = name.chars().take(keep).collect();
        }
        let base = session::join(&folder, &name);
        let mut path = format!("{}.ngc", base);
        let mut i = 1;
        while session::resolve(&self.config_dir, &path).exists() {
            path = format!("{}_{}.ngc", base, i);
            i += 1;
        }
        Ok(path)
    }
}

/// What the JOB page shows for a probing op: its measure settings and,
/// for the op it checks, that op's tool and whether the tool's diameter
/// sets the feature (so a correction can be offered).
/// A TOOL CHECK op's line under its name: which tool it will find.
fn tool_check_detail(job: &crate::model::Job, index: usize) -> String {
    let enabled: std::collections::HashMap<String, bool> = job.ops.iter().map(|o| (o.id.clone(), o.enabled)).collect();
    match crate::compiler::checked_tool(job, index, &enabled) {
        Some((t, j)) => format!("CHECKS T{} (FROM OP {}) ON THE SETTER  ·  STOPS THE JOB IF IT IS BROKEN", t, j + 1),
        None => "NO CUTTING TOOL BEFORE IT TO CHECK".into(),
    }
}

fn probe_snap(op: &crate::model::Op, job: &crate::model::Job, machine: &Option<crate::compiler::Machine>) -> Value {
    let Ok(p) = crate::probe::parse(op) else { return Value::Null };
    let Some(m) = p.measure else { return json!({"mode": p.mode, "routine": p.routine}) };
    let checked = m.op.as_deref().and_then(|id| job.ops.iter().position(|o| o.id == id));
    let (tool, sized) = match checked.map(|i| &job.ops[i]) {
        Some(o) if o.kind == "conversational" => {
            let t = o.params.get("tool_number").and_then(py::as_number).map(|t| t as i64);
            let sized = o.params.get("type").and_then(|v| v.as_str()) == Some("helical_bore");
            (t, sized && crate::probe::inside(&p.routine).is_some())
        }
        Some(o) if o.kind == "cam" => (o.params.get("setup").and_then(|s| s.get("tool")).and_then(py::as_number).map(|t| t as i64), false),
        _ => (None, false),
    };
    let diameter = tool.and_then(|t| machine.as_ref().and_then(|m| m.tool(t)).map(|t| t.diameter));
    json!({
        "mode": p.mode, "routine": p.routine, "value": m.value, "values": crate::probe::values_for(&p.routine),
        "nominal": m.nominal, "tol": m.tol, "pause": m.pause,
        "op": m.op, "op_pos": checked.map(|i| i + 1), "op_name": checked.map(|i| job.ops[i].name.to_uppercase()),
        "tool": tool, "diameter": diameter, "sized": sized,
    })
}
