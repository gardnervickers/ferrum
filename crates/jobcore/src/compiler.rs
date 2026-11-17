//! Compile a job into one LinuxCNC program (`mymill_job/compiler.py`) and
//! turn conversational ops into a setup and a body
//! (`mymill_job/conversational.py`).

use crate::gcode::{self, comment_text, fmt, oword, strip_comment, words};
use crate::mill_ops::{GenError, Generator, Kind, MACHINE_TOP};
use crate::model::{Job, Op, Setup, MAX_OPS};
use crate::py::{self, Error, Result};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const HAL_PREFIX: &str = "mymill-job";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tool {
    pub number: i64,
    pub diameter: f64,
    pub length: f64,
}

/// What the compiler needs to know about the mill.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub tools: BTreeMap<i64, Tool>,
    pub probe_tool: i64,
    pub max_forward_rpm: Option<f64>,
    pub max_reverse_rpm: Option<f64>,
    pub blend_tolerance_mm: f64,
    pub sync_input: i64,
}

impl Machine {
    pub fn new(tools: BTreeMap<i64, Tool>, probe_tool: i64, max_forward_rpm: Option<f64>, max_reverse_rpm: Option<f64>) -> Machine {
        Machine { tools, probe_tool, max_forward_rpm, max_reverse_rpm, blend_tolerance_mm: 0.001, sync_input: 0 }
    }

    pub fn tool(&self, number: i64) -> Option<&Tool> {
        self.tools.get(&number)
    }

    /// `tools.get(value)` for a JSON value (3 and 3.0 find the same tool).
    pub fn tool_for(&self, v: &Value) -> Option<&Tool> {
        let n = py::as_number(v)?;
        if n.fract() != 0.0 {
            return None;
        }
        self.tools.get(&(n as i64))
    }

    pub fn max_rpm(&self, direction: &str) -> Option<f64> {
        if direction == "CCW" {
            self.max_reverse_rpm
        } else {
            self.max_forward_rpm
        }
    }
}

/// `read_tool_table`.
pub fn read_tool_table(path: &std::path::Path, shown: &str) -> Result<BTreeMap<i64, Tool>> {
    let text = py::read_text(path, shown)?;
    let mut tools = BTreeMap::new();
    for line in py::file_lines(&text) {
        let rest = line.trim_start_matches(py::is_space);
        let Some(rest) = rest.strip_prefix('T') else { continue };
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let after = &rest[digits.len()..];
        // \b after the digits: the next character must not be a word character.
        if after.chars().next().map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false) {
            continue;
        }
        let number: i64 = digits.parse().map_err(|_| Error::value_error("bad tool number"))?;
        let fields_text = after.split(';').next().unwrap_or("");
        let mut fields: HashMap<String, String> = HashMap::new();
        for w in py::split(fields_text) {
            let mut chars = w.chars();
            let first = chars.next().unwrap();
            fields.insert(first.to_uppercase().collect(), chars.collect());
        }
        let num = |key: &str| -> Result<f64> {
            match fields.get(key) {
                Some(v) if !v.is_empty() => py::parse_float(v),
                _ => Ok(0.0),
            }
        };
        tools.insert(number, Tool { number, diameter: num("D")?, length: num("Z")? });
    }
    Ok(tools)
}

/// One op's place in the built program.
#[derive(Clone, Debug, PartialEq)]
pub struct OpRange {
    pub position: usize,
    pub op_id: String,
    pub name: String,
    pub first_line: i64,
    pub last_line: i64,
    pub setup: Option<Setup>,
    pub stop_line: Option<i64>,
}

/// Tool diameters a build depends on, keyed by `str(tool number)`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Depends {
    pub tools: BTreeMap<String, f64>,
}

impl Depends {
    /// `json.dumps(depends, sort_keys=True)`.
    pub fn dumps(&self) -> String {
        let items: Vec<String> = self
            .tools
            .iter()
            .map(|(k, d)| format!("\"{}\": {{\"diameter\": {}}}", k, py::dump_float(*d)))
            .collect();
        format!("{{\"tools\": {{{}}}}}", items.join(", "))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledJob {
    pub text: String,
    pub stamp: String,
    pub ranges: Vec<OpRange>,
    pub depends: Depends,
    pub warnings: Vec<String>,
}

impl CompiledJob {
    pub fn stamp_pin_value(&self) -> i64 {
        stamp_pin_value(&self.stamp)
    }
}

pub fn stamp_pin_value(stamp: &str) -> i64 {
    let v = i64::from_str_radix(&stamp[..7], 16).unwrap_or(0);
    if v == 0 {
        1
    } else {
        v
    }
}

pub fn pin(name: &str) -> String {
    format!("#<_hal[{}.{}]>", HAL_PREFIX, name)
}

/// `STAMP_LINE.match(line)`: the stamp, if the line is a stamp line.
pub fn stamp_line(line: &str) -> Option<&str> {
    let body = line.strip_suffix('\n').unwrap_or(line);
    let inner = body.strip_prefix("(MYMILL-JOB STAMP=")?.strip_suffix(')')?;
    if inner.len() == 12 && inner.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        Some(inner)
    } else {
        None
    }
}

fn op_error(op: &Op, e: &str) -> String {
    format!("Op \"{}\": {}", op.name, e)
}

pub fn compile_job(job: &Job, machine: &Machine) -> Result<CompiledJob> {
    let mut errors = job.problems();
    if !errors.is_empty() {
        return Err(Error::Job(errors));
    }
    if job.ops.len() > MAX_OPS {
        return Err(Error::job(format!("A job can hold at most {} ops.", MAX_OPS)));
    }
    let mut resolved: Vec<(&Op, Option<Setup>, Vec<String>)> = Vec::new();
    for op in &job.ops {
        let (setup, body) = match resolve(op, machine) {
            Ok(r) => r,
            Err(Error::Job(e)) => {
                errors.extend(e);
                continue;
            }
            Err(other) => return Err(other),
        };
        if let Some(s) = &setup {
            errors.extend(setup_problems(s, machine).iter().map(|e| op_error(op, e)));
            let limit = machine.max_rpm(s.spindle_dir.as_str().unwrap_or(""));
            errors.extend(body_problems(&body, s, limit).iter().map(|e| op_error(op, e)));
        }
        resolved.push((op, setup, body));
    }
    if !errors.is_empty() {
        return Err(Error::Job(errors));
    }

    let mut lines: Vec<String> = vec![
        format!("(MYMILL-JOB {})", comment_text(&job.name)),
        "(Built by the JOB page. Do not edit: the JOB page checks the stamp.)".into(),
        "G21 G90 G94 G17 G40 G49 G80 G91.1 G97".into(),
        "o<job_stamp> if [#<_task> EQ 1]".into(),
        format!("  o<job_stamp_check> if [{} NE {{STAMP_PIN}}]", pin("stamp")),
        "    (abort, The JOB page does not match this program. Rebuild the job.)".into(),
        "  o<job_stamp_check> endif".into(),
        "o<job_stamp> endif".into(),
    ];
    let total = resolved.len();
    let mut ranges = Vec::new();
    for (i, (op, setup, body)) in resolved.iter().enumerate() {
        let position = i + 1;
        let first = lines.len() as i64 + 2;
        let block = wrap(position, total, op, setup.as_ref(), body, machine);
        let stop_line = if position > 1 {
            let marker = format!("o<job_op{}_stop> endif", position);
            let stop_index = block.iter().position(|l| *l == marker).unwrap() - 1;
            Some(first + stop_index as i64)
        } else {
            None
        };
        lines.extend(block);
        ranges.push(OpRange {
            position,
            op_id: op.id.clone(),
            name: op.name.clone(),
            first_line: first,
            last_line: lines.len() as i64 + 1,
            setup: setup.clone(),
            stop_line,
        });
    }
    lines.push("(MYMILL-JOB END)".into());
    lines.push(MACHINE_TOP.into());
    lines.push("M5".into());
    lines.push("M9".into());
    lines.push("M30".into());

    let used: BTreeSet<i64> = resolved.iter().filter_map(|(_, s, _)| s.as_ref().map(|s| s.tool_number())).collect();
    let mut depends = Depends::default();
    for n in used {
        depends.tools.insert(n.to_string(), machine.tool(n).map(|t| t.diameter).unwrap_or(0.0));
    }
    let stamp = make_stamp(&lines, &depends);
    let pin_value = stamp_pin_value(&stamp).to_string();
    let mut text_lines = vec![format!("(MYMILL-JOB STAMP={})", stamp)];
    text_lines.extend(lines.iter().map(|l| l.replace("{STAMP_PIN}", &pin_value)));
    let text = text_lines.join("\n") + "\n";
    let enabled: HashMap<String, bool> = job.ops.iter().map(|o| (o.id.clone(), o.enabled)).collect();
    let warnings = dependency_problems(job, &enabled);
    Ok(CompiledJob { text, stamp, ranges, depends, warnings })
}

fn resolve(op: &Op, machine: &Machine) -> Result<(Option<Setup>, Vec<String>)> {
    match op.kind.as_str() {
        "conversational" => {
            let (setup, body) = build_conversational(op, machine)?;
            Ok((Some(setup), body))
        }
        "cam" => {
            let p = &op.params;
            let body = match p.get("body") {
                Some(Value::Array(a)) if a.iter().all(|x| x.is_string()) => {
                    a.iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>()
                }
                _ => return Err(Error::job(format!("Op \"{}\": body must be a list of lines.", op.name))),
            };
            if body.is_empty() {
                return Err(Error::job(format!("Op \"{}\": body is empty.", op.name)));
            }
            let setup = Setup::from_dict(p.get("setup").unwrap_or(&Value::Null)).map_err(|e| match e {
                Error::Job(errs) => Error::job(format!("Op \"{}\": {}", op.name, errs.join("\n"))),
                other => other,
            })?;
            Ok((Some(setup), body))
        }
        "probe" => {
            crate::probe::parse(op)?;
            Ok((None, vec![]))
        }
        "step" => {
            let ok = matches!(op.params.get("message"), Some(Value::String(m)) if !comment_text(m).is_empty());
            if !ok {
                return Err(Error::job(format!("Op \"{}\": a manual step needs a message.", op.name)));
            }
            Ok((None, vec![]))
        }
        other => Err(Error::job(format!("Op \"{}\": unknown kind {}.", op.name, py::repr_str(other)))),
    }
}

fn setup_problems(setup: &Setup, machine: &Machine) -> Vec<String> {
    let mut errors = setup.problems();
    if !errors.is_empty() {
        return errors;
    }
    let tool = setup.tool_number();
    if machine.tool(tool).is_none() {
        errors.push(format!("T{} is not in the tool table.", tool));
    }
    if tool == machine.probe_tool {
        errors.push(format!("T{} is the probe. A cutting op cannot use it.", tool));
    }
    if let Some(limit) = machine.max_rpm(Setup::str_field(&setup.spindle_dir)).filter(|l| *l != 0.0) {
        if setup.rpm_f() > limit {
            errors.push(format!("{} rpm is above the spindle maximum of {}.", fmt(setup.rpm_f()), fmt(limit)));
        }
    }
    errors
}

fn body_problems(body: &[String], setup: &Setup, limit: Option<f64>) -> Vec<String> {
    let limit = limit.filter(|l| *l != 0.0);
    let mut errors = Vec::new();
    let mut depth: i64 = 0;
    for (n, line) in body.iter().enumerate() {
        let place = format!("body line {} ({})", n + 1, py::strip(line));
        let stripped = strip_comment(line);
        let text = py::strip(&stripped);
        if text.is_empty() {
            continue;
        }
        if text.starts_with('%') {
            errors.push(format!("{} is a program delimiter.", place));
            continue;
        }
        if let Some((label, keyword)) = oword(text) {
            if label.starts_with("<job_") {
                errors.push(format!("{} uses a name reserved for the job.", place));
            }
            if keyword == "sub" || keyword == "endsub" {
                errors.push(format!("{} defines a subroutine inside an op.", place));
            }
            if keyword == "do" {
                errors.push(format!("{} uses a do-while loop, which the job cannot check.", place));
            } else if matches!(keyword.as_str(), "if" | "while" | "repeat") {
                depth += 1;
            } else if matches!(keyword.as_str(), "endif" | "endwhile" | "endrepeat") {
                depth -= 1;
            }
        }
        let line_words = words(text);
        for (letter, value) in &line_words {
            let Some(value) = *value else {
                if "GMTH".contains(*letter) {
                    errors.push(format!("{}: {} is computed, so it cannot be checked.", place, letter));
                } else if *letter == 'S' {
                    errors.push(format!("{}: the spindle speed is computed, so it cannot be checked.", place));
                }
                continue;
            };
            if let Some(why) = gcode::forbidden(*letter, value) {
                errors.push(format!("{}: {}{} {}.", place, letter, fmt(value), why));
            } else if *letter == 'T' {
                errors.push(format!("{}: T words belong to the job, not the op body.", place));
            } else if *letter == 'G' && gcode::units_code(value).map(|u| !py::eq(&Value::String(u.into()), &setup.units)).unwrap_or(false) {
                errors.push(format!("{}: switches units away from {}.", place, py::str_of(&setup.units)));
            } else if *letter == 'G' && gcode::wcs_code(value).map(|w| !py::eq(&Value::String(w.into()), &setup.wcs)).unwrap_or(false) {
                errors.push(format!("{}: switches work offset away from {}.", place, py::str_of(&setup.wcs)));
            } else if *letter == 'H'
                && line_words.contains(&('G', Some(43.0)))
                && !py::eq(&Value::Number((value.trunc() as i64).into()), &setup.tool)
            {
                errors.push(format!("{}: applies a length offset for another tool.", place));
            } else if *letter == 'S' && limit.map(|l| value > l).unwrap_or(false) {
                errors.push(format!("{}: S{} is above the spindle maximum of {}.", place, fmt(value), fmt(limit.unwrap())));
            }
        }
    }
    if depth != 0 {
        errors.push("The body opens and closes O-word blocks unevenly.".into());
    }
    errors
}

fn wrap(position: usize, total: usize, op: &Op, setup: Option<&Setup>, body: &[String], machine: &Machine) -> Vec<String> {
    let tag = format!("op{}", position);
    let mut out = vec![format!("(MYMILL-OP {}/{} ID={} {})", position, total, op.id.to_uppercase(), comment_text(&op.name))];
    out.push(format!("M66 E{} L0", machine.sync_input));
    out.push("#<_job_run> = 1".into());
    out.push("#<_job_stop> = 0".into());
    out.push(format!("o<job_{}_flags> if [#<_task> EQ 1]", tag));
    out.push(format!("  #<_job_run> = [{} EQ 0]", pin(&format!("skip-{}", position))));
    if position > 1 {
        out.push(format!("  #<_job_stop> = [{} NE 0]", pin("stop-after")));
    }
    out.push(format!("o<job_{}_flags> endif", tag));
    if position > 1 {
        out.push(format!("o<job_{}_stop> if [#<_job_stop>]", tag));
        out.push("  M5".into());
        out.push("  M9".into());
        out.push(format!("  (MSG, Stopped before op {}: {})", position, comment_text(&op.name)));
        out.push("  M0".into());
        out.push(format!("o<job_{}_stop> endif", tag));
    }
    out.push(format!("o<job_{}> if [#<_job_run>]", tag));
    out.push(format!("  {}", MACHINE_TOP));
    match setup {
        None if op.kind == "probe" => {
            // Checked in resolve().
            let body = crate::probe::body(op, position, &tag, machine.probe_tool).unwrap_or_default();
            out.extend(body.iter().map(|l| format!("  {}", l)));
        }
        None => {
            out.push("  M5".into());
            out.push("  M9".into());
            let message = op.params.get("message").and_then(|m| m.as_str()).unwrap_or("");
            out.push(format!("  (MSG, {})", comment_text(message)));
            out.push("  M0".into());
        }
        Some(s) => {
            let units = Setup::str_field(&s.units);
            let units_code = if units == "IN" { "G20" } else { "G21" };
            let blend = if units == "IN" { machine.blend_tolerance_mm / 25.4 } else { machine.blend_tolerance_mm };
            let tool = s.tool_number();
            out.push(format!("  {} G90 G94 G17 G40 G80 G91.1 G97 G64 P{}", units_code, fmt(blend)));
            out.push(format!("  o<job_{}_tool> if [#<_current_tool> NE {}]", tag, tool));
            out.push("    M5".into());
            out.push("    M9".into());
            out.push(format!("    T{} M6", tool));
            out.push(format!("  o<job_{}_tool> endif", tag));
            out.push(format!("  G43 H{}", tool));
            out.push(format!("  {}", Setup::str_field(&s.wcs)));
            let dir = if Setup::str_field(&s.spindle_dir) == "CCW" { "M4" } else { "M3" };
            out.push(format!("  S{} {}", fmt(s.rpm_f()), dir));
            let coolant = match Setup::str_field(&s.coolant) {
                "MIST" => "M7",
                "FLOOD" => "M8",
                _ => "M9",
            };
            out.push(format!("  {}", coolant));
            if let Some(feed) = py::as_number(&s.feed) {
                out.push(format!("  F{}", fmt(feed)));
            }
            out.extend(body.iter().map(|l| format!("  {}", l)));
            out.push("  G80".into());
            out.push("  G40".into());
            out.push(format!("  {}", MACHINE_TOP));
        }
    }
    out.push(format!("o<job_{}> endif", tag));
    out
}

fn make_stamp(lines: &[String], depends: &Depends) -> String {
    let mut data = lines.join("\n").into_bytes();
    data.extend_from_slice(depends.dumps().as_bytes());
    py::sha256_hex(&data)[..12].to_string()
}

/// Warnings for ops that will run without something they need.
pub fn dependency_problems(job: &Job, enabled: &HashMap<String, bool>) -> Vec<String> {
    let mut providers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, op) in job.ops.iter().enumerate() {
        for p in &op.provides {
            providers.entry(p.as_str()).or_default().push(i);
        }
    }
    let on = |id: &str| *enabled.get(id).unwrap_or(&false);
    let mut warnings = Vec::new();
    for (i, op) in job.ops.iter().enumerate() {
        if !on(&op.id) {
            continue;
        }
        for need in needs(op) {
            if op.provides.contains(&need) {
                continue;
            }
            if let Some(other) = need.strip_prefix("op:") {
                match job.index_of(other) {
                    None => warnings.push(format!("\"{}\" needs an op that is no longer in the job.", op.name)),
                    Some(j) if j > i => {
                        warnings.push(format!("\"{}\" needs \"{}\", which comes after it.", op.name, job.ops[j].name))
                    }
                    Some(j) if !on(other) => {
                        warnings.push(format!("\"{}\" needs \"{}\", which is skipped.", op.name, job.ops[j].name))
                    }
                    _ => {}
                }
                continue;
            }
            let Some(list) = providers.get(need.as_str()) else { continue };
            let before: Vec<usize> = list.iter().copied().filter(|j| *j < i).collect();
            if before.is_empty() {
                warnings.push(format!("\"{}\" needs {}, which is only set by a later op.", op.name, label(&need)));
            } else if !before.iter().any(|j| on(&job.ops[*j].id)) {
                let names: Vec<String> = before.iter().map(|j| format!("\"{}\"", job.ops[*j].name)).collect();
                warnings.push(format!(
                    "\"{}\" needs {}, which {} would set, but it is skipped.",
                    op.name,
                    label(&need),
                    names.join(", ")
                ));
            }
        }
    }
    warnings
}

fn needs(op: &Op) -> Vec<String> {
    let mut needs = op.needs.clone();
    if op.kind == "probe" {
        // A FIND op sets its work offset rather than needing it.
        return needs;
    }
    let wcs = match op.kind.as_str() {
        "conversational" => op.params.get("wcs").cloned(),
        "cam" => match op.params.get("setup") {
            Some(Value::Object(s)) => s.get("wcs").cloned(),
            _ => None,
        },
        _ => None,
    };
    if let Some(w) = wcs {
        if py::truthy(&w) {
            needs.push(format!("wcs:{}", py::str_of(&w)));
        }
    }
    needs
}

fn label(need: &str) -> String {
    match need.strip_prefix("wcs:") {
        Some(w) => format!("work offset {}", w),
        None => need.to_string(),
    }
}

pub fn read_stamp(text: &str) -> Option<String> {
    let first = text.split('\n').next().unwrap_or("");
    stamp_line(first).map(|s| s.to_string())
}

/// True if `text` is exactly what the compiler wrote for `depends`.
pub fn verify(text: &str, depends: &Depends) -> bool {
    let Some(stamp) = read_stamp(text) else { return false };
    let mut body: Vec<&str> = text.split('\n').skip(1).collect();
    if body.last() == Some(&"") {
        body.pop();
    }
    let pin_value = stamp_pin_value(&stamp).to_string();
    let marker = pin("stamp");
    let original: Vec<String> = body
        .iter()
        .map(|l| if l.contains(&marker) { l.replace(&pin_value, "{STAMP_PIN}") } else { l.to_string() })
        .collect();
    make_stamp(&original, depends) == stamp
}

/// Why a built program no longer matches the tool table.
pub fn stale_reasons(depends: &Depends, machine: &Machine) -> Vec<String> {
    let mut items: Vec<(&String, &f64)> = depends.tools.iter().collect();
    items.sort_by_key(|(k, _)| k.parse::<i64>().unwrap_or(0));
    let mut reasons = Vec::new();
    for (number, diameter) in items {
        match machine.tool(number.parse().unwrap_or(0)) {
            None => reasons.push(format!("T{} has been removed from the tool table.", number)),
            Some(tool) => {
                if (tool.diameter - diameter).abs() > 1e-6 {
                    reasons.push(format!(
                        "T{} diameter changed from {} to {}.",
                        number,
                        fmt(*diameter),
                        fmt(tool.diameter)
                    ));
                }
            }
        }
    }
    reasons
}

/// The op range containing a 1-based program line.
pub fn op_at_line(ranges: &[OpRange], line: i64) -> Option<&OpRange> {
    ranges.iter().find(|r| r.first_line <= line && line <= r.last_line)
}

// -- conversational ops ----------------------------------------------------------

pub const COMMON_FIELDS: [&str; 12] = [
    "wcs", "coolant", "units", "tool_number", "spindle_rpm", "spindle_dir", "z_start", "z_end", "retract",
    "z_feed", "z_clear", "xy_feed",
];

/// Fields each op type takes on top of COMMON_FIELDS, and optional ones.
pub fn type_fields(kind: Kind) -> (&'static [&'static str], &'static [&'static str], bool) {
    match kind {
        Kind::Face => (&["step_over", "step_down", "x_start", "x_end", "y_start", "y_end"], &[], true),
        Kind::Drill => (&["holes", "retract_mode", "cycle"], &["cycle_param"], false),
        Kind::HelicalBore => (&["center", "diameter", "ramp_angle"], &[], true),
        Kind::ThreadMill => (&["center", "diameter", "pitch", "radial_passes", "hand"], &["pre_drill"], true),
    }
}

fn gen_error(op: &Op, e: GenError) -> Error {
    match e {
        GenError::Operation(errs) => Error::Job(errs.iter().map(|x| op_error(op, x)).collect()),
        GenError::Py(e) => e,
    }
}

/// `conversational.build`: `(Setup, body lines)` for a conversational op.
pub fn build_conversational(op: &Op, machine: &Machine) -> Result<(Setup, Vec<String>)> {
    let mut params = op.params.clone();
    let op_type = params.remove("type").unwrap_or(Value::Null);
    let kind = op_type.as_str().and_then(Kind::from_type).ok_or_else(|| {
        Error::job(format!("Op \"{}\": unknown conversational type {}.", op.name, py::repr(&op_type)))
    })?;
    let (fields, optional, needs_diameter) = type_fields(kind);
    let required: BTreeSet<&str> = COMMON_FIELDS.iter().chain(fields.iter()).copied().collect();
    let allowed: BTreeSet<&str> = required.iter().chain(optional.iter()).copied().collect();
    let missing: Vec<&str> = required.iter().copied().filter(|k| !params.contains_key(*k)).collect();
    let unknown: Vec<&str> = params.keys().map(|k| k.as_str()).filter(|k| !allowed.contains(k)).collect();
    let mut errors = Vec::new();
    if !missing.is_empty() {
        errors.push(op_error(op, &format!("missing field(s) {}", missing.join(", "))));
    }
    if !unknown.is_empty() {
        errors.push(op_error(op, &format!("unknown field(s) {}", unknown.join(", "))));
    }
    if !errors.is_empty() {
        return Err(Error::Job(errors));
    }

    let mut gen = Generator::new(kind);
    let cycle = params.remove("cycle").unwrap_or(Value::Null);
    let cycle_param = params.remove("cycle_param").unwrap_or(py::float_value(0.0));
    for (key, value) in &params {
        gen.set_attr(key, value)?;
    }
    let tool = machine.tool_for(&params["tool_number"]).copied();
    if needs_diameter {
        gen.tool_diameter = tool.map(|t| t.diameter).unwrap_or(0.0);
    }
    gen.max_spindle_rpm = machine.max_rpm(params["spindle_dir"].as_str().unwrap_or(""));
    let cycle_text = match &cycle {
        Value::String(s) => s.clone(),
        other => py::str_of(other),
    };
    let param = match kind {
        Kind::Drill => py::as_number(&cycle_param).ok_or_else(|| {
            Error::type_error(format!(
                "'>' not supported between instances of '{}' and 'int'",
                py::type_name(&cycle_param)
            ))
        })?,
        _ => 0.0,
    };
    let lines = gen.lines(&cycle_text, param).map_err(|e| gen_error(op, e))?;
    let setup = Setup::typed(
        &py::strip_upper(&gen.units),
        &gen.wcs,
        gen.tool_number.trunc() as i64,
        gen.spindle_rpm,
        &py::strip_upper(&gen.spindle_dir),
        &py::strip_upper(&gen.coolant),
        Some(gen.xy_feed),
    );
    let body = strip_wrapper(op, &gen, &lines)?;
    Ok((setup, body))
}

fn parse_xy(line: &str) -> Option<(f64, f64)> {
    let mut x = None;
    let mut y = None;
    let mut keys = BTreeSet::new();
    for p in py::split(line).iter().skip(1) {
        let mut chars = p.chars();
        let key = chars.next()?;
        let value = py::parse_float(chars.as_str()).ok()?;
        keys.insert(key);
        match key {
            'X' => x = Some(value),
            'Y' => y = Some(value),
            _ => {}
        }
    }
    if !line.starts_with("G0 ") || keys != BTreeSet::from(['X', 'Y']) {
        return None;
    }
    Some((x?, y?))
}

fn strip_wrapper(op: &Op, gen: &Generator, lines: &[String]) -> Result<Vec<String>> {
    let header = || Error::job(format!("Op \"{}\": the generator output has an unexpected header.", op.name));
    let feed_at = lines.iter().position(|l| l.starts_with('F')).ok_or_else(header)?;
    let first_move = lines.get(feed_at + 1).ok_or_else(header)?;
    let xy = parse_xy(first_move).ok_or_else(header)?;
    let head = gen.start_op(xy)?;
    if lines.len() < head.len() || lines[..head.len()] != head[..] {
        return Err(Error::job(format!("Op \"{}\": the generator header does not match the job contract.", op.name)));
    }
    if !head.iter().take(2).any(|l| l == MACHINE_TOP) {
        return Err(Error::job(format!("Op \"{}\": the generator no longer lifts to the machine top first.", op.name)));
    }
    let tail = gen.end_op();
    if lines.len() < tail.len() || lines[lines.len() - tail.len()..] != tail[..] {
        return Err(Error::job(format!(
            "Op \"{}\": the generator closing lines do not match the job contract.",
            op.name
        )));
    }
    let end = lines.len() - tail.len() + 1;
    Ok(if feed_at + 1 <= end { lines[feed_at + 1..end].to_vec() } else { vec![] })
}

/// `params_from_generator`: the fields a job stores, read back from a
/// configured generator.
pub fn params_from_generator(
    gen: &Generator,
    cycle: Option<&Value>,
    cycle_param: Option<&Value>,
) -> Result<Map<String, Value>> {
    let op_type = match gen.kind {
        Kind::Face => "face",
        Kind::Drill => "drill",
        Kind::HelicalBore => "helical_bore",
        Kind::ThreadMill => "thread_mill",
    };
    let (fields, optional, _) = type_fields(gen.kind);
    let mut params = Map::new();
    params.insert("type".into(), Value::String(op_type.into()));
    for key in COMMON_FIELDS.iter().chain(fields.iter()).chain(optional.iter()) {
        let key = *key;
        if key == "cycle" {
            params.insert(key.into(), cycle.cloned().unwrap_or(Value::Null));
            continue;
        }
        if key == "cycle_param" {
            if let Some(cp) = cycle_param.filter(|v| !v.is_null()) {
                params.insert(key.into(), py::float_value(py::to_float(cp)?));
            }
            continue;
        }
        let Some(value) = gen.get_attr(key) else { continue };
        let value = match key {
            "tool_number" | "radial_passes" => {
                Value::Number((py::as_number(&value).unwrap_or(0.0).trunc() as i64).into())
            }
            _ => match value {
                Value::String(s) => Value::String(py::strip_upper(&s)),
                other => other,
            },
        };
        if value.is_null() && optional.contains(&key) {
            continue;
        }
        params.insert(key.into(), value);
    }
    Ok(params)
}

/// A short name for the op list, built from its own fields.
pub fn default_name(params: &Map<String, Value>) -> String {
    let n = |v: f64| {
        let t = py::fixed(v, 3);
        let t = t.trim_end_matches('0').trim_end_matches('.').to_string();
        if t.is_empty() { "0".to_string() } else { t }
    };
    let num = |key: &str| params.get(key).and_then(py::as_number).unwrap_or(0.0);
    let tool = format!("T{}", params.get("tool_number").map(py::str_of).unwrap_or_else(|| "?".into()));
    match params.get("type").and_then(|v| v.as_str()) {
        Some("face") => format!(
            "FACE {} X {} {}",
            n((num("x_end") - num("x_start")).abs()),
            n((num("y_start") - num("y_end")).abs()),
            tool
        ),
        Some("drill") => {
            let count = params.get("holes").and_then(|h| h.as_array()).map(|a| a.len()).unwrap_or(0);
            let cycle = params.get("cycle").map(py::str_of).unwrap_or_else(|| "DRILL".into());
            format!("{} {} HOLE{} {}", cycle, count, if count == 1 { "" } else { "S" }, tool)
        }
        Some("helical_bore") => format!("BORE D{} {}", n(num("diameter")), tool),
        Some("thread_mill") => format!(
            "THREAD {} X {} {} {}",
            n(num("diameter")),
            n(num("pitch")),
            params.get("hand").map(py::str_of).unwrap_or_else(|| "RH".into()),
            tool
        ),
        _ => format!("OP {}", tool),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Op;

    fn machine(extra: &[(i64, f64, f64)]) -> Machine {
        let mut tools = BTreeMap::new();
        for (n, d, l) in [(1, 6.0, 100.0), (3, 8.0, 90.0), (7, 4.2, 110.0), (14, 50.0, 67.6), (99, 2.0, 120.0)]
            .iter()
            .chain(extra.iter())
        {
            tools.insert(*n, Tool { number: *n, diameter: *d, length: *l });
        }
        Machine::new(tools, 99, Some(4250.0), Some(4250.0))
    }

    fn params(text: &str) -> Map<String, Value> {
        py::loads(text).unwrap().as_object().unwrap().clone()
    }

    const COMMON: &str = r#""wcs": "G54", "coolant": "FLOOD", "units": "MM", "spindle_dir": "CW", "z_start": 0.0, "retract": 2.0, "z_feed": 100.0, "z_clear": 10.0, "xy_feed": 600.0"#;

    fn face(id: &str) -> Op {
        let p = params(&format!(
            r#"{{"type": "face", "step_over": 35.0, "step_down": 1.0, "x_start": 0.0, "x_end": 100.0, "y_start": 60.0, "y_end": 0.0, "tool_number": 14, "spindle_rpm": 3200.0, "z_end": -2.0, {}}}"#,
            COMMON
        ));
        Op::new(id.into(), "Face top".into(), "conversational", p)
    }

    fn drill(id: &str) -> Op {
        let p = params(&format!(
            r#"{{"type": "drill", "holes": [[10, 10], [90, 10]], "retract_mode": "G98", "cycle": "PECK", "cycle_param": 2.0, "tool_number": 7, "spindle_rpm": 1800.0, "z_end": -12.0, {}}}"#,
            COMMON
        ));
        Op::new(id.into(), "Drill holes".into(), "conversational", p)
    }

    fn bore(id: &str) -> Op {
        let p = params(&format!(
            r#"{{"type": "helical_bore", "center": [50, 30], "diameter": 12.0, "ramp_angle": 2.0, "tool_number": 1, "spindle_rpm": 4000.0, "z_end": -6.0, {}}}"#,
            COMMON
        ));
        Op::new(id.into(), "Bore".into(), "conversational", p)
    }

    fn cam(id: &str, name: &str, body: &[&str]) -> Op {
        let mut p = params(r#"{"source": "part.ngc", "setup": {"units": "MM", "wcs": "G54", "tool": 3, "rpm": 4000.0, "spindle_dir": "CW", "coolant": "FLOOD", "feed": null}}"#);
        p.insert("body".into(), Value::Array(body.iter().map(|l| Value::String(l.to_string())).collect()));
        Op::new(id.into(), name.into(), "cam", p)
    }

    fn step(id: &str) -> Op {
        Op::new(id.into(), "Flip part".into(), "step", params(r#"{"message": "Flip the part and re-clamp"}"#))
    }

    fn sample() -> Job {
        Job {
            name: "Bracket".into(),
            ops: vec![
                face("00000001"),
                cam("00000002", "Adaptive", &["G0 X10 Y10", "G0 Z5", "G1 Z-1 F300", "G1 X40 F900", "G0 Z15"]),
                step("00000003"),
                drill("00000004"),
                bore("00000005"),
            ],
        }
    }

    fn probe_op(id: &str, mode: &str, link: Option<&str>) -> Op {
        let mut p = params(
            r#"{"mode": "find", "routine": "bore", "label": "BORE CENTER", "calls": ["o<probe_round_pocket_center_start> call"], "values": {"3025": 12.0}, "wcs": "G54", "start": [50.0, 30.0, 3.0]}"#,
        );
        p.insert("mode".into(), Value::String(mode.into()));
        if mode == "measure" {
            let m = format!(r#"{{"value": "diameter", "nominal": 12.0, "tol": 0.02, "op": {}, "on_fail": "pause"}}"#, link.map_or("null".to_string(), |l| format!("\"{l}\"")));
            p.insert("measure".into(), py::loads(&m).unwrap());
        }
        Op::new(id.into(), "Bore center".into(), "probe", p)
    }

    #[test]
    fn a_job_finds_cuts_and_checks() {
        let job = Job {
            name: "Probed".into(),
            ops: vec![probe_op("0000000a", "find", None), bore("0000000b"), probe_op("0000000c", "measure", Some("0000000b"))],
        };
        let compiled = compile_job(&job, &machine(&[])).unwrap();
        let t = &compiled.text;
        assert_eq!(compiled.ranges.len(), 3);
        assert!(t.contains("(MYMILL-PROBE"), "{t}");
        assert!(t.contains("o<probe_round_pocket_center_start> call"));
        assert!(t.contains("(DEBUG, MYMILL-MEASURE ID=0000000c VALUE=#<_job_m>)"), "{t}");
        // The find writes the offset; the check measures only.
        assert_eq!(t.matches("#3030 = 0").count(), 1);
        assert_eq!(t.matches("#3030 = 1").count(), 1);
        // Each probing op loads the probe and ends at the op contract's Z.
        assert!(t.matches("G43 H99").count() >= 2);
        if std::env::var_os("JOBCORE_PRINT").is_some() {
            println!("{t}");
        }
    }

    #[test]
    fn ranges_cover_each_op() {
        let compiled = compile_job(&sample(), &machine(&[])).unwrap();
        let lines: Vec<&str> = compiled.text.split('\n').collect();
        let mut previous = 0;
        for r in &compiled.ranges {
            assert!(lines[r.first_line as usize - 1].starts_with(&format!("(MYMILL-OP {}/", r.position)));
            assert_eq!(lines[r.last_line as usize - 1], format!("o<job_op{}> endif", r.position));
            assert!(r.first_line > previous);
            previous = r.last_line;
            match r.stop_line {
                None => assert_eq!(r.position, 1),
                Some(s) => assert_eq!(lines[s as usize - 1], "  M0"),
            }
        }
        assert!(op_at_line(&compiled.ranges, 1).is_none());
        assert!(compiled.text.contains(&format!("#<_hal[mymill-job.stamp]> NE {}", compiled.stamp_pin_value())));
    }

    #[test]
    fn stamp_detects_edits_and_diameters_but_not_lengths() {
        let job = sample();
        let a = compile_job(&job, &machine(&[])).unwrap();
        assert_eq!(read_stamp(&a.text).as_deref(), Some(a.stamp.as_str()));
        assert!(verify(&a.text, &a.depends));
        assert!(!verify(&a.text.replacen("F600", "F6000", 1), &a.depends));
        assert!(!verify("G0 X0\n", &a.depends));
        let b = compile_job(&job, &machine(&[(1, 6.5, 100.0)])).unwrap();
        assert_ne!(a.stamp, b.stamp);
        assert!(!verify(&a.text, &b.depends));
        let c = compile_job(&job, &machine(&[(3, 8.0, 91.0)])).unwrap();
        assert_eq!(c.stamp, a.stamp);
        assert_eq!(compile_job(&job, &machine(&[])).unwrap().text, a.text);
    }

    #[test]
    fn stale_reasons_name_the_tool() {
        let compiled = compile_job(&sample(), &machine(&[])).unwrap();
        assert!(stale_reasons(&compiled.depends, &machine(&[])).is_empty());
        assert!(stale_reasons(&compiled.depends, &machine(&[(7, 4.2, 112.5)])).is_empty());
        assert_eq!(
            stale_reasons(&compiled.depends, &machine(&[(7, 4.0, 110.0)])),
            vec!["T7 diameter changed from 4.2 to 4.".to_string()]
        );
        let mut removed = machine(&[]);
        removed.tools.remove(&1);
        assert_eq!(stale_reasons(&compiled.depends, &removed), vec!["T1 has been removed from the tool table.".to_string()]);
    }

    #[test]
    fn refusals() {
        let cases = [
            ("M30", "ends the program"),
            ("T5 M6", "changes tool"),
            ("G55", "switches work offset away from G54"),
            ("G20", "switches units away from MM"),
            ("G43 H7", "applies a length offset for another tool"),
            ("S5000 M3", "above the spindle maximum"),
            ("#<x> = [1 + 2] T4", "T words belong to the job"),
            ("N100 G10 L20 P1 X0", "writes offsets"),
        ];
        for (line, reason) in cases {
            let job = Job { name: "j".into(), ops: vec![cam("00000001", "A", &["G0 X0 Y0", line, "G0 Z10"])] };
            match compile_job(&job, &machine(&[])) {
                Err(Error::Job(errors)) => assert!(errors.iter().any(|e| e.contains(reason)), "{:?}", errors),
                other => panic!("{} compiled: {:?}", line, other.is_ok()),
            }
        }
        for line in ["G54", "G21", "G43 H3", "S4000 M3", "(T5 M6 in a comment)", "G91 G0 Z1"] {
            let job = Job { name: "j".into(), ops: vec![cam("00000001", "A", &["G0 X0 Y0", line, "G0 Z10"])] };
            assert!(compile_job(&job, &machine(&[])).is_ok(), "{}", line);
        }
    }

    #[test]
    fn dependencies() {
        let mut probe = cam("00000001", "Find stock", &["G0 X0 Y0"]);
        probe.provides = vec!["wcs:G54".into()];
        let job = Job { name: "j".into(), ops: vec![probe.clone(), face("00000002")] };
        let mut enabled: HashMap<String, bool> = job.ops.iter().map(|o| (o.id.clone(), true)).collect();
        assert!(dependency_problems(&job, &enabled).is_empty());
        enabled.insert("00000001".into(), false);
        assert_eq!(
            dependency_problems(&job, &enabled),
            vec!["\"Face top\" needs work offset G54, which \"Find stock\" would set, but it is skipped.".to_string()]
        );
        let job = Job { name: "j".into(), ops: vec![face("00000002"), probe] };
        let enabled: HashMap<String, bool> = job.ops.iter().map(|o| (o.id.clone(), true)).collect();
        assert_eq!(
            dependency_problems(&job, &enabled),
            vec!["\"Face top\" needs work offset G54, which is only set by a later op.".to_string()]
        );
    }

    #[test]
    fn default_names_and_captured_params() {
        assert_eq!(default_name(&face("00000001").params), "FACE 100 X 60 T14");
        assert_eq!(default_name(&drill("00000001").params), "PECK 2 HOLES T7");
        assert_eq!(default_name(&bore("00000001").params), "BORE D12 T1");
        for op in [face("00000001"), drill("00000002"), bore("00000003")] {
            let (_, body) = build_conversational(&op, &machine(&[])).unwrap();
            assert!(body[0].starts_with("G0 X"));
            let mut p = op.params.clone();
            let kind = Kind::from_type(p.remove("type").unwrap().as_str().unwrap()).unwrap();
            let mut gen = Generator::new(kind);
            for (k, v) in &p {
                gen.set_attr(k, v).unwrap();
            }
            let back = params_from_generator(&gen, p.get("cycle"), p.get("cycle_param")).unwrap();
            assert!(py::eq(&Value::Object(back), &Value::Object(op.params.clone())), "{}", op.name);
        }
    }
}
