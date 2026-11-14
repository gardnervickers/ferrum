//! The job model (`mymill_job/model.py`): an ordered list of ops saved as a
//! small JSON file.

use crate::py::{self, Error, Result};
use serde_json::{Map, Value};

pub const FORMAT: &str = "mymill-job";
pub const VERSION: i64 = 1;
pub const KINDS: [&str; 3] = ["conversational", "cam", "step"];
pub const UNITS: [&str; 2] = ["MM", "IN"];
pub const WCS_CODES: [&str; 9] = ["G54", "G55", "G56", "G57", "G58", "G59", "G59.1", "G59.2", "G59.3"];
pub const COOLANT: [&str; 3] = ["OFF", "MIST", "FLOOD"];
pub const SPINDLE_DIRS: [&str; 2] = ["CW", "CCW"];
pub const MAX_OPS: usize = 64;

/// `_ID.match`: eight lowercase hex digits (Python's `$` also allows one
/// trailing newline).
pub fn valid_id(id: &str) -> bool {
    let body = id.strip_suffix('\n').unwrap_or(id);
    body.len() == 8 && body.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One operation in a job.
#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub params: Map<String, Value>,
    pub enabled: bool,
    pub needs: Vec<String>,
    pub provides: Vec<String>,
}

impl Op {
    pub fn new(id: String, name: String, kind: &str, params: Map<String, Value>) -> Op {
        Op { id, name, kind: kind.into(), params, enabled: true, needs: vec![], provides: vec![] }
    }

    pub fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), Value::String(self.id.clone()));
        m.insert("name".into(), Value::String(self.name.clone()));
        m.insert("kind".into(), Value::String(self.kind.clone()));
        m.insert("enabled".into(), Value::Bool(self.enabled));
        m.insert("needs".into(), Value::Array(self.needs.iter().cloned().map(Value::String).collect()));
        m.insert("provides".into(), Value::Array(self.provides.iter().cloned().map(Value::String).collect()));
        m.insert("params".into(), Value::Object(self.params.clone()));
        Value::Object(m)
    }

    pub fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !valid_id(&self.id) {
            errors.push("id must be 8 lowercase hex characters".into());
        }
        if py::strip(&self.name).is_empty() {
            errors.push("name must not be empty".into());
        }
        if !KINDS.contains(&self.kind.as_str()) {
            errors.push(format!("kind must be one of {}", KINDS.join(", ")));
        }
        errors
    }

    /// `Op.from_dict`.
    pub fn from_dict(data: &Value) -> Result<Op> {
        let d = require_keys("op", data, &["id", "name", "kind", "params"], &["enabled", "needs", "provides"])?;
        let id = &d["id"];
        let name = &d["name"];
        let kind = &d["kind"];
        let params = &d["params"];
        let enabled = d.get("enabled").cloned().unwrap_or(Value::Bool(true));
        let empty = Value::Array(vec![]);
        let mut errors = Vec::new();
        if !matches!(id, Value::String(s) if valid_id(s)) {
            errors.push("id must be 8 lowercase hex characters".to_string());
        }
        if !matches!(name, Value::String(s) if !py::strip(s).is_empty()) {
            errors.push("name must not be empty".into());
        }
        if !KINDS.iter().any(|k| matches!(kind, Value::String(s) if s == k)) {
            errors.push(format!("kind must be one of {}", KINDS.join(", ")));
        }
        if !enabled.is_boolean() {
            errors.push("enabled must be true or false".into());
        }
        if !params.is_object() {
            errors.push("params must be an object".into());
        }
        for key in ["needs", "provides"] {
            let v = d.get(key).unwrap_or(&empty);
            let ok = matches!(v, Value::Array(a) if a.iter().all(|x| matches!(x, Value::String(s) if !s.is_empty())));
            if !ok {
                errors.push(format!("{} must be a list of names", key));
            }
        }
        if !errors.is_empty() {
            let shown = py::str_of(name);
            return Err(Error::Job(errors.into_iter().map(|e| format!("Op \"{}\": {}", shown, e)).collect()));
        }
        let strings = |key: &str| -> Vec<String> {
            d.get(key)
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
                .unwrap_or_default()
        };
        Ok(Op {
            id: id.as_str().unwrap().into(),
            name: name.as_str().unwrap().into(),
            kind: kind.as_str().unwrap().into(),
            params: params.as_object().unwrap().clone(),
            enabled: enabled.as_bool().unwrap(),
            needs: strings("needs"),
            provides: strings("provides"),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub name: String,
    pub ops: Vec<Op>,
}

impl Job {
    pub fn op(&self, op_id: &Value) -> Result<&Op> {
        self.ops.iter().find(|op| matches!(op_id, Value::String(s) if *s == op.id)).ok_or_else(|| Error::key(op_id))
    }

    pub fn op_mut(&mut self, op_id: &Value) -> Result<&mut Op> {
        self.ops.iter_mut().find(|op| matches!(op_id, Value::String(s) if *s == op.id)).ok_or_else(|| Error::key(op_id))
    }

    pub fn index(&self, op_id: &Value) -> Result<usize> {
        self.ops.iter().position(|op| matches!(op_id, Value::String(s) if *s == op.id)).ok_or_else(|| Error::key(op_id))
    }

    pub fn index_of(&self, op_id: &str) -> Option<usize> {
        self.ops.iter().position(|op| op.id == op_id)
    }

    /// Move an op up (negative) or down (positive), clamped to the list.
    pub fn move_op(&mut self, op_id: &Value, offset: i64) -> Result<usize> {
        let i = self.index(op_id)?;
        let j = 0.max((self.ops.len() as i64 - 1).min(i as i64 + offset)) as usize;
        let op = self.ops.remove(i);
        let j = j.min(self.ops.len());
        self.ops.insert(j, op);
        Ok(j)
    }

    pub fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if py::strip(&self.name).is_empty() {
            errors.push("The job needs a name.".to_string());
        }
        if self.ops.is_empty() {
            errors.push("The job has no ops.".into());
        }
        if self.ops.len() > MAX_OPS {
            errors.push(format!("A job can hold at most {} ops.", MAX_OPS));
        }
        let mut seen = std::collections::HashSet::new();
        for op in &self.ops {
            for e in op.problems() {
                errors.push(format!("Op \"{}\": {}", op.name, e));
            }
            if !seen.insert(op.id.clone()) {
                errors.push(format!("Op id {} appears twice.", op.id));
            }
        }
        errors
    }

    pub fn to_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("format".into(), Value::String(FORMAT.into()));
        m.insert("version".into(), Value::Number(VERSION.into()));
        m.insert("name".into(), Value::String(self.name.clone()));
        m.insert("ops".into(), Value::Array(self.ops.iter().map(Op::to_dict).collect()));
        Value::Object(m)
    }

    /// `json.dumps(to_dict(), indent=2, sort_keys=True) + '\n'`.
    pub fn dumps(&self) -> String {
        py::dumps_indent(&self.to_dict()) + "\n"
    }

    pub fn from_dict(data: &Value) -> Result<Job> {
        let format_ok = matches!(data, Value::Object(m) if m.get("format") == Some(&Value::String(FORMAT.into())));
        if !format_ok {
            return Err(Error::job("This is not a mymill job file."));
        }
        let m = data.as_object().unwrap();
        let version = m.get("version").cloned().unwrap_or(Value::Null);
        if !py::eq(&version, &Value::Number(VERSION.into())) {
            return Err(Error::job(format!(
                "Job file version {} is not supported (expected {}).",
                py::str_of(&version),
                VERSION
            )));
        }
        require_keys("job", data, &["format", "version", "name", "ops"], &[])?;
        let ops = match &m["ops"] {
            Value::Array(a) => a,
            _ => return Err(Error::job("ops must be a list")),
        };
        let ops = ops.iter().map(Op::from_dict).collect::<Result<Vec<_>>>()?;
        let name = &m["name"];
        let mut errors = Vec::new();
        let name_text = match name {
            Value::String(s) if !py::strip(s).is_empty() => s.clone(),
            _ => {
                errors.push("The job needs a name.".to_string());
                String::new()
            }
        };
        let job = Job { name: name_text, ops };
        for e in job.problems() {
            if e != "The job needs a name." {
                errors.push(e);
            }
        }
        if !errors.is_empty() {
            return Err(Error::Job(errors));
        }
        Ok(job)
    }

    pub fn loads(text: &str) -> Result<Job> {
        let data = py::loads(text).map_err(|e| Error::job(format!("The job file is not valid JSON: {}", e)))?;
        Job::from_dict(&data)
    }
}

/// `_require_keys`: returns the object when it is one with the right keys.
pub fn require_keys<'a>(
    what: &str,
    data: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>> {
    let m = match data {
        Value::Object(m) => m,
        _ => return Err(Error::job(format!("{} must be an object", what))),
    };
    let missing: Vec<&str> = required.iter().copied().filter(|k| !m.contains_key(*k)).collect();
    let mut unknown: Vec<&str> =
        m.keys().map(|k| k.as_str()).filter(|k| !required.contains(k) && !optional.contains(k)).collect();
    unknown.sort();
    let mut errors = Vec::new();
    if !missing.is_empty() {
        errors.push(format!("{} is missing {}", what, missing.join(", ")));
    }
    if !unknown.is_empty() {
        errors.push(format!("{} has unknown field(s) {}", what, unknown.join(", ")));
    }
    if !errors.is_empty() {
        return Err(Error::Job(errors));
    }
    Ok(m)
}

/// `_positive`.
pub fn positive(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.as_f64().map(|f| f > 0.0).unwrap_or(false))
}

/// What an op needs in place before its first move. Values stay as JSON
/// because a CAM op's setup comes straight from the job file and its
/// problems are reported before anything is typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub units: Value,
    pub wcs: Value,
    pub tool: Value,
    pub rpm: Value,
    pub spindle_dir: Value,
    pub coolant: Value,
    pub feed: Value,
}

fn in_list(v: &Value, list: &[&str]) -> bool {
    matches!(v, Value::String(s) if list.contains(&s.as_str()))
}

impl Setup {
    pub fn typed(units: &str, wcs: &str, tool: i64, rpm: f64, spindle_dir: &str, coolant: &str, feed: Option<f64>) -> Setup {
        Setup {
            units: Value::String(units.into()),
            wcs: Value::String(wcs.into()),
            tool: Value::Number(tool.into()),
            rpm: py::float_value(rpm),
            spindle_dir: Value::String(spindle_dir.into()),
            coolant: Value::String(coolant.into()),
            feed: feed.map(py::float_value).unwrap_or(Value::Null),
        }
    }

    pub fn problems(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !in_list(&self.units, &UNITS) {
            errors.push(format!("units must be one of {}", UNITS.join(", ")));
        }
        if !in_list(&self.wcs, &WCS_CODES) {
            errors.push(format!("work offset must be one of {}", WCS_CODES.join(", ")));
        }
        let tool_ok = py::is_int(&self.tool) && self.tool.as_f64().map(|t| t > 0.0).unwrap_or(false);
        if !tool_ok {
            errors.push("tool must be a tool number of 1 or more".into());
        }
        if !positive(&self.rpm) {
            errors.push("spindle speed must be greater than 0".into());
        }
        if !in_list(&self.spindle_dir, &SPINDLE_DIRS) {
            errors.push("spindle direction must be CW or CCW".into());
        }
        if !in_list(&self.coolant, &COOLANT) {
            errors.push(format!("coolant must be one of {}", COOLANT.join(", ")));
        }
        if !self.feed.is_null() && !positive(&self.feed) {
            errors.push("feed must be greater than 0".into());
        }
        errors
    }

    pub fn from_dict(data: &Value) -> Result<Setup> {
        let m = require_keys("setup", data, &["units", "wcs", "tool", "rpm"], &["spindle_dir", "coolant", "feed"])?;
        Ok(Setup {
            units: m["units"].clone(),
            wcs: m["wcs"].clone(),
            tool: m["tool"].clone(),
            rpm: m["rpm"].clone(),
            spindle_dir: m.get("spindle_dir").cloned().unwrap_or(Value::String("CW".into())),
            coolant: m.get("coolant").cloned().unwrap_or(Value::String("OFF".into())),
            feed: m.get("feed").cloned().unwrap_or(Value::Null),
        })
    }

    pub fn tool_number(&self) -> i64 {
        self.tool.as_i64().unwrap_or(0)
    }
    pub fn rpm_f(&self) -> f64 {
        py::as_number(&self.rpm).unwrap_or(0.0)
    }
    pub fn str_field(v: &Value) -> &str {
        v.as_str().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        py::loads(r#"{"format": "mymill-job", "version": 1, "name": "j", "ops": [
            {"id": "0000000a", "name": "s", "kind": "step", "params": {"message": "m"}}]}"#)
        .unwrap()
    }

    fn errors_of(v: &Value) -> Vec<String> {
        match Job::from_dict(v) {
            Err(Error::Job(e)) => e,
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn bad_job_files_are_refused() {
        let mut d = sample();
        d["version"] = 2.into();
        assert!(errors_of(&d)[0].contains("version 2 is not supported"));
        let mut d = sample();
        d["format"] = "other".into();
        assert!(errors_of(&d)[0].contains("not a mymill job file"));
        let mut d = sample();
        d["ops"][0]["colour"] = "red".into();
        assert!(errors_of(&d)[0].contains("unknown field(s) colour"));
        let mut d = sample();
        d["ops"][0]["id"] = "XYZ".into();
        assert!(errors_of(&d)[0].contains("id must be 8 lowercase hex"));
        let mut d = sample();
        d["ops"][0]["kind"] = "laser".into();
        assert!(errors_of(&d)[0].contains("kind must be one of"));
        let mut d = sample();
        let op = d["ops"][0].clone();
        d["ops"].as_array_mut().unwrap().push(op);
        assert!(errors_of(&d).iter().any(|e| e.contains("appears twice")));
        let mut d = sample();
        d["version"] = py::loads("1.0").unwrap();
        assert!(Job::from_dict(&d).is_ok());
    }

    #[test]
    fn round_trip_and_move() {
        let job = Job::from_dict(&sample()).unwrap();
        assert_eq!(Job::loads(&job.dumps()).unwrap(), job);
        let mut job = Job {
            name: "j".into(),
            ops: (0..4).map(|i| Op::new(format!("0000000{}", i), format!("s{}", i), "step", Map::new())).collect(),
        };
        let first = Value::String("00000000".into());
        assert_eq!(job.move_op(&first, -3).unwrap(), 0);
        assert_eq!(job.move_op(&first, 2).unwrap(), 2);
        assert_eq!(job.ops[2].id, "00000000");
        assert_eq!(job.move_op(&first, 100).unwrap(), 3);
    }
}
