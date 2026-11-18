//! Probing ops: a probe routine run as a step of a job.
//!
//! The PROBE screen makes them (ADD TO JOB): the macro call it would run,
//! the per-run `#30xx` values, the work system, and where the probe stood
//! when the op was added (the start). In the job the op loads the probe,
//! rapids over the start at the top of travel, feeds down to it with a
//! guarded move (G38.3, stops on contact and aborts), runs the routine and
//! goes back to the top.
//!
//! FIND writes the work offset (#3030 = 0), as on the PROBE screen.
//! MEASURE reports only (#3030 = 1). With a nominal it compares the result:
//! it prints a `MYMILL-MEASURE` line the JOB page reads, and when the value
//! is out of tolerance it can pause at the top (M0) with a message.
//!
//! Params (`params` of an op of kind "probe"):
//! - `mode`: "find" | "measure"
//! - `routine`: what the PROBE screen picked (stock, corner, icorner, edge,
//!   top, boss, bore, pocket, slot, ridge), for names and checks
//! - `label`: the PROBE screen's name for it ("BORE CENTER")
//! - `calls`: macro call lines, run in order (auto stock is two)
//! - `values`: `{"3019": 5.0, ...}` per-run probe settings
//! - `wcs`: "G54".."G59.3"
//! - `start`: [x, y, z] in `wcs`, where the probe stood
//! - `start_machine` (FIND): the same spot in machine coordinates. A FIND
//!   op goes there with G53: it sets `wcs`, so a start kept in `wcs` would
//!   move with every run (and be wrong after the first one). A MEASURE op
//!   starts at `start`: the feature sits where the work offset says.
//! - `measure` (MEASURE only): `{"value": "diameter"|"x"|"y"|"z",
//!   "nominal": number|null, "tol": number, "op": op id|null,
//!   "on_fail": "pause"|"continue"}`

use crate::gcode::{comment_text, fmt};
use crate::mill_ops::machine_top;
use crate::model::{Op, WCS_CODES};
use crate::py::{Error, Result};
use serde_json::{json, Map, Value};

pub const ROUTINES: [&str; 10] = ["stock", "corner", "icorner", "edge", "top", "boss", "bore", "pocket", "slot", "ridge"];
/// What MEASURE can check, by routine.
pub fn values_for(routine: &str) -> &'static [&'static str] {
    match routine {
        "boss" | "bore" => &["diameter"],
        "pocket" => &["x", "y"],
        "slot" | "ridge" => &["x", "y"],
        "top" => &["z"],
        _ => &[],
    }
}

/// The macro result parameter for a measured value (Probe Basic's
/// routines leave them in #1001–#1010).
fn result_param(value: &str) -> &'static str {
    match value {
        "diameter" => "#1010",
        "x" => "#1001",
        "y" => "#1003",
        _ => "#1009",
    }
}

/// Whether a bigger tool makes the feature bigger (bore, pocket, slot)
/// or smaller (boss, ridge); None when the tool size doesn't set it.
pub fn inside(routine: &str) -> Option<bool> {
    match routine {
        "bore" | "pocket" | "slot" => Some(true),
        "boss" | "ridge" => Some(false),
        _ => None,
    }
}

fn bad(op: &Op, why: &str) -> Error {
    Error::job(format!("Op \"{}\": {}", op.name, why))
}

/// A macro call the job may run: `o<probe_...> call` with numeric args.
fn valid_call(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("o<") else { return false };
    let Some((name, args)) = rest.split_once("> call") else { return false };
    let name_ok = (name.starts_with("probe_") || name.starts_with("mymill_probe_"))
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    let mut a = args.trim();
    while !a.is_empty() {
        let Some(r) = a.strip_prefix('[') else { return false };
        let Some((num, r)) = r.split_once(']') else { return false };
        if num.trim().parse::<f64>().is_err() {
            return false;
        }
        a = r.trim_start();
    }
    name_ok
}

pub struct Parsed {
    pub mode: String,
    pub routine: String,
    pub label: String,
    pub calls: Vec<String>,
    pub values: Vec<(u16, f64)>,
    pub wcs: String,
    pub start: [f64; 3],
    /// FIND: the start in machine coordinates (ops made before this was
    /// kept have none and start at `start`).
    pub start_machine: Option<[f64; 3]>,
    pub measure: Option<Measure>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    pub value: String,
    pub nominal: Option<f64>,
    pub tol: f64,
    pub op: Option<String>,
    pub pause: bool,
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(|v| v.as_f64()).filter(|x| x.is_finite())
}

pub fn parse(op: &Op) -> Result<Parsed> {
    let p = &op.params;
    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mode = s("mode");
    if mode != "find" && mode != "measure" {
        return Err(bad(op, "mode must be find or measure."));
    }
    let routine = s("routine");
    if !ROUTINES.contains(&routine.as_str()) {
        return Err(bad(op, &format!("a job cannot run the probe routine \"{routine}\".")));
    }
    let calls: Vec<String> = match p.get("calls") {
        Some(Value::Array(a)) => a.iter().map(|c| c.as_str().unwrap_or("").trim().to_string()).collect(),
        _ => vec![],
    };
    if calls.is_empty() || !calls.iter().all(|c| valid_call(c)) {
        return Err(bad(op, "calls must be probe macro calls."));
    }
    let mut values = Vec::new();
    if let Some(Value::Object(m)) = p.get("values") {
        for (k, v) in m {
            let n = k.parse::<u16>().ok().filter(|n| (3004..=3037).contains(n));
            match (n, num(Some(v))) {
                (Some(n), Some(v)) if n != 3030 && n != 3014 => values.push((n, v)),
                _ => return Err(bad(op, &format!("probe setting #{k} cannot be set by a job."))),
            }
        }
    }
    let wcs = s("wcs");
    if !WCS_CODES.contains(&wcs.as_str()) {
        return Err(bad(op, "wcs must be G54..G59.3."));
    }
    let xyz = |key: &str| -> Result<Option<[f64; 3]>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(a)) if a.len() == 3 => {
                let v: Vec<f64> = a.iter().filter_map(|x| num(Some(x))).collect();
                if v.len() != 3 {
                    return Err(bad(op, &format!("{key} must be three numbers.")));
                }
                Ok(Some([v[0], v[1], v[2]]))
            }
            _ => Err(bad(op, &format!("{key} must be [x, y, z]."))),
        }
    };
    let start = xyz("start")?.ok_or_else(|| bad(op, "start must be [x, y, z]."))?;
    let start_machine = if mode == "find" { xyz("start_machine")? } else { None };
    // The op comes down from machine Z 0; a start above that cannot be.
    if start_machine.is_some_and(|m| m[2] > 0.0) {
        return Err(bad(op, "start_machine Z must be at or below machine Z 0."));
    }
    let measure = if mode == "measure" {
        let m = p.get("measure").and_then(|m| m.as_object()).cloned().unwrap_or_default();
        let value = m.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !values_for(&routine).contains(&value.as_str()) {
            return Err(bad(op, &format!("{routine} cannot measure \"{value}\".")));
        }
        let nominal = num(m.get("nominal"));
        let tol = num(m.get("tol")).unwrap_or(0.0);
        if nominal.is_some() && !(tol > 0.0) {
            return Err(bad(op, "the tolerance must be more than 0."));
        }
        let link = m.get("op").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
        let pause = m.get("on_fail").and_then(|v| v.as_str()) != Some("continue");
        Some(Measure { value, nominal, tol, op: link, pause })
    } else {
        None
    };
    Ok(Parsed { mode, routine, label: s("label"), calls, values, wcs, start, start_machine, measure })
}

/// Default MEASURE settings for a routine.
pub fn default_measure(routine: &str) -> Value {
    let value = values_for(routine).first().copied().unwrap_or("");
    json!({"value": value, "nominal": Value::Null, "tol": 0.02, "op": Value::Null, "on_fail": "pause"})
}

/// The op's lines between the job's MACHINE_TOP lines (two-space indent
/// added by the caller). `position` names the op in messages; `tag` is the
/// op's o-word stem.
pub fn body(op: &Op, position: usize, tag: &str, probe_tool: i64) -> Result<Vec<String>> {
    let p = parse(op)?;
    let mut out = vec![
        format!("(MYMILL-PROBE {} {})", p.mode.to_uppercase(), comment_text(&p.label)),
        "M5".into(),
        "M9".into(),
        "G21 G90 G94 G17 G40 G80 G91.1 G49".into(),
        format!("o<job_{tag}_tool> if [#<_current_tool> NE {probe_tool}]"),
        format!("  T{probe_tool} M6"),
        format!("o<job_{tag}_tool> endif"),
        format!("G43 H{probe_tool}"),
        p.wcs.clone(),
    ];
    // Per-run settings: kept, set, and put back after (the app also puts
    // its defaults back when a job stops part way).
    let mut set: Vec<(u16, f64)> = p.values.clone();
    set.push((3030, if p.mode == "find" { 0.0 } else { 1.0 }));
    for (n, _) in &set {
        out.push(format!("#<_job_p{n}> = #{n}"));
    }
    for (n, v) in &set {
        out.push(format!("#{n} = {}", fmt(*v)));
    }
    // Down to the start, stopping on any contact.
    match p.start_machine {
        Some([x, y, z]) => {
            // From the top of travel (the tool change may have left Z
            // anywhere), so the way down is the start's machine Z.
            out.push(machine_top());
            out.push(format!("G53 G0 X{} Y{}", fmt(x), fmt(y)));
            out.push("F[#3017]".into());
            out.push(format!("G91 G38.3 Z{}", fmt(z)));
            out.push("G90".into());
        }
        None => {
            let [x, y, z] = p.start;
            out.push(format!("G0 X{} Y{}", fmt(x), fmt(y)));
            out.push("F[#3017]".into());
            out.push(format!("G38.3 Z{}", fmt(z)));
        }
    }
    out.push("o<mymill_probe_assert_no_contact> call".into());
    if let Some(m) = &p.measure {
        out.push(format!("{} = -999999", result_param(&m.value)));
    }
    // Only on the machine: the preview's interpreter has no probe, so the
    // routines would abort there ("Probe event did not record point data")
    // and hide the rest of the job from the preview and the travel check.
    out.push(format!("o<job_{tag}_live> if [#<_task> EQ 1]"));
    out.extend(p.calls.iter().map(|c| format!("  {c}")));
    out.push(format!("o<job_{tag}_live> endif"));
    // Several routines end in G91.
    out.push("G90".into());
    for (n, _) in &set {
        out.push(format!("#{n} = #<_job_p{n}>"));
    }
    out.push(machine_top());
    if let Some(m) = &p.measure {
        out.push(format!("#<_job_m> = {}", result_param(&m.value)));
        out.push(format!("(DEBUG, MYMILL-MEASURE ID={} VALUE=#<_job_m>)", op.id));
        if let (Some(nominal), true) = (m.nominal, m.pause) {
            out.push(format!("o<job_{tag}_tol> if [ABS[#<_job_m> - {}] GT {}]", fmt(nominal), fmt(m.tol)));
            out.push(format!(
                "  (MSG, Op {position} {}: out of tolerance. The JOB page shows the reading. CYCLE START goes on, STOP ends the job.)",
                comment_text(&op.name)
            ));
            out.push("  M0".into());
            out.push(format!("o<job_{tag}_tol> endif"));
        }
    }
    Ok(out)
}

/// The work offset a FIND op sets ("wcs:G54"), for the job's dependency
/// warnings.
pub fn provides(op: &Op) -> Vec<String> {
    match parse(op) {
        Ok(p) if p.mode == "find" => vec![format!("wcs:{}", p.wcs)],
        _ => vec![],
    }
}

/// The ops a MEASURE op checks ("op:<id>").
pub fn needs(op: &Op) -> Vec<String> {
    match parse(op) {
        Ok(Parsed { measure: Some(Measure { op: Some(id), .. }), .. }) => vec![format!("op:{id}")],
        _ => vec![],
    }
}

/// One line for the JOB page.
pub fn detail(op: &Op) -> String {
    let Ok(p) = parse(op) else { return String::new() };
    // The name already says what is probed.
    let start = match p.start_machine {
        Some(m) => format!("STARTS AT MACHINE X{} Y{} Z{}", fmt(round3(m[0])), fmt(round3(m[1])), fmt(round3(m[2]))),
        None => format!("STARTS AT X{} Y{} Z{}", fmt(round3(p.start[0])), fmt(round3(p.start[1])), fmt(round3(p.start[2]))),
    };
    match &p.measure {
        None => format!("SETS {}  ·  {}", p.wcs, start),
        Some(m) => {
            let what = match m.value.as_str() {
                "diameter" => "Ø".to_string(),
                "z" => "Z".to_string(),
                v => format!("{} WIDTH", v.to_uppercase()),
            };
            let spec = match m.nominal {
                Some(n) => format!("{what} {} ±{}", fmt(round3(n)), fmt(round3(m.tol))),
                None => format!("{what} (NO NOMINAL)"),
            };
            format!("MEASURES {}  ·  {}  ·  {}", spec, p.wcs, start)
        }
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Change a MEASURE op's settings from the JOB page. Keys: nominal (number
/// or null), tol, value, op (id or null), on_fail.
pub fn set_measure(op: &mut Op, changes: &Map<String, Value>) -> Result<()> {
    if op.kind != "probe" {
        return Err(bad(op, "only probing ops have measure settings."));
    }
    let routine = op.params.get("routine").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut m = op.params.get("measure").and_then(|m| m.as_object()).cloned().unwrap_or_else(|| default_measure(&routine).as_object().unwrap().clone());
    for (k, v) in changes {
        if !["nominal", "tol", "value", "op", "on_fail"].contains(&k.as_str()) {
            return Err(bad(op, &format!("unknown measure setting {k}.")));
        }
        m.insert(k.clone(), v.clone());
    }
    let link = m.get("op").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
    let mut new = op.clone();
    new.params.insert("measure".into(), Value::Object(m));
    parse(&new)?;
    new.needs.retain(|n| !n.starts_with("op:"));
    if let Some(id) = link {
        new.needs.push(format!("op:{id}"));
    }
    *op = new;
    Ok(())
}

/// The tool diameter that would have cut `nominal` where `measured` came
/// out, for a feature cut by the tool's side.
pub fn corrected_diameter(routine: &str, diameter: f64, measured: f64, nominal: f64) -> Option<f64> {
    let err = measured - nominal;
    match inside(routine)? {
        // A bore comes out as big as the tool really is, relative to the
        // tool the path was made for.
        true => Some(diameter + err),
        false => Some(diameter - err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(mode: &str) -> Op {
        let mut params = Map::new();
        params.insert("mode".into(), json!(mode));
        params.insert("routine".into(), json!("bore"));
        params.insert("label".into(), json!("BORE CENTER"));
        params.insert("calls".into(), json!(["o<probe_round_pocket_center_start> call"]));
        params.insert("values".into(), json!({"3025": 10.0, "3019": 2.5}));
        params.insert("wcs".into(), json!("G54"));
        params.insert("start".into(), json!([10.0, 20.0, -3.0]));
        if mode == "measure" {
            params.insert("measure".into(), json!({"value": "diameter", "nominal": 10.0, "tol": 0.02, "op": null, "on_fail": "pause"}));
        }
        Op::new("a1b2c3d4".into(), "Bore check".into(), "probe", params)
    }

    #[test]
    fn calls_must_be_probe_macros() {
        assert!(valid_call("o<probe_auto_stock_center> call [2] [150.000] [8]"));
        assert!(valid_call("o<probe_z_minus_wco> call"));
        assert!(!valid_call("o<go_to_home> call"));
        assert!(!valid_call("o<probe_x> call [#5420]"));
        assert!(!valid_call("o<probe_x> call [1]\nG0 X0"));
    }

    #[test]
    fn find_body_loads_the_probe_and_guards_the_approach() {
        let b = body(&op("find"), 2, "op2", 99).unwrap();
        let t = b.join("\n");
        assert!(t.contains("T99 M6"));
        assert!(t.contains("G43 H99"));
        assert!(t.contains("#3030 = 0"));
        assert!(t.contains("#<_job_p3025> = #3025"));
        assert!(t.contains("#3025 = 10\n"));
        let rapid = b.iter().position(|l| l == "G0 X10 Y20").unwrap();
        let guard = b.iter().position(|l| l == "G38.3 Z-3").unwrap();
        let call = b.iter().position(|l| l == "  o<probe_round_pocket_center_start> call").unwrap();
        // The routine runs on the machine only, not in the preview.
        assert_eq!(b[call - 1], "o<job_op2_live> if [#<_task> EQ 1]");
        assert_eq!(b[call + 1], "o<job_op2_live> endif");
        let restore = b.iter().position(|l| l == "#3025 = #<_job_p3025>").unwrap();
        assert!(rapid < guard && guard < call && call < restore);
        assert_eq!(b.last().unwrap(), &machine_top());
        assert!(!t.contains("MYMILL-MEASURE"));
        assert_eq!(provides(&op("find")), vec!["wcs:G54"]);
    }

    #[test]
    fn find_starts_at_the_taught_machine_position() {
        let mut o = op("find");
        o.params.insert("start_machine".into(), json!([215.0, -150.0, -130.0]));
        let b = body(&o, 1, "op1", 99).unwrap();
        let at = |l: &str| b.iter().position(|x| x == l).unwrap_or_else(|| panic!("no {l} in {b:?}"));
        // Top of travel, over the start, guarded down by the machine Z.
        assert_eq!(b[at("G53 G0 X215 Y-150") - 1], machine_top());
        assert!(at("G43 H99") < at("G53 G0 X215 Y-150"));
        assert_eq!(at("G91 G38.3 Z-130"), at("G53 G0 X215 Y-150") + 2);
        assert_eq!(at("G90"), at("G91 G38.3 Z-130") + 1);
        assert!(!b.iter().any(|l| l == "G0 X10 Y20"));
        assert!(detail(&o).contains("STARTS AT MACHINE X215 Y-150 Z-130"), "{}", detail(&o));
        // A check starts in its work system even if a machine start is given.
        let mut m = op("measure");
        m.params.insert("start_machine".into(), json!([215.0, -150.0, -130.0]));
        assert!(body(&m, 1, "op1", 99).unwrap().iter().any(|l| l == "G0 X10 Y20"));
        // Above machine Z 0 there is nowhere to come down from.
        o.params.insert("start_machine".into(), json!([215.0, -150.0, 1.0]));
        assert!(parse(&o).is_err());
    }

    #[test]
    fn measure_reports_and_pauses_out_of_tolerance() {
        let b = body(&op("measure"), 3, "op3", 99).unwrap();
        let t = b.join("\n");
        assert!(t.contains("#3030 = 1"));
        assert!(t.contains("#<_job_m> = #1010"));
        assert!(t.contains("(DEBUG, MYMILL-MEASURE ID=a1b2c3d4 VALUE=#<_job_m>)"));
        assert!(t.contains("o<job_op3_tol> if [ABS[#<_job_m> - 10] GT 0.02]"));
        assert!(provides(&op("measure")).is_empty());
    }

    #[test]
    fn settings_a_job_may_not_change() {
        let mut o = op("find");
        o.params.insert("values".into(), json!({"3030": 1}));
        assert!(parse(&o).is_err());
        let mut o = op("find");
        o.params.insert("routine".into(), json!("nose"));
        assert!(parse(&o).is_err());
    }

    #[test]
    fn set_measure_links_the_checked_op() {
        let mut o = op("measure");
        let mut ch = Map::new();
        ch.insert("op".into(), json!("deadbeef"));
        ch.insert("nominal".into(), json!(12.0));
        set_measure(&mut o, &ch).unwrap();
        assert_eq!(o.needs, vec!["op:deadbeef"]);
        assert!(detail(&o).contains("Ø 12 ±0.02"));
        let mut ch = Map::new();
        ch.insert("tol".into(), json!(0));
        assert!(set_measure(&mut o, &ch).is_err());
    }

    #[test]
    fn corrections_follow_the_side_the_tool_cuts() {
        // A bore 0.03 small: the tool cuts as if 0.03 smaller.
        assert!((corrected_diameter("bore", 6.0, 9.97, 10.0).unwrap() - 5.97).abs() < 1e-9);
        // A boss 0.03 big: the tool cuts as if 0.03 smaller too.
        assert!((corrected_diameter("boss", 6.0, 20.03, 20.0).unwrap() - 5.97).abs() < 1e-9);
        assert_eq!(corrected_diameter("top", 6.0, 1.0, 0.0), None);
    }
}
