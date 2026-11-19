//! Generates `mirror.rs` from the `Live` and `Actions` globals in state.slint.
//!
//! The generated code (included by the app and the web build) provides:
//! - `PROPS`: the mirrored `Live` property names (images excluded);
//! - `get_prop(&Live, name) -> Option<Value>` and `set_prop(&Live, name, &Value)`;
//! - `invoke_action(&Actions, name, &[Value]) -> bool`;
//! - `forward_actions(&Actions, Rc<dyn Fn(&str, Vec<Value>)>, skip)`.
//! Values are `serde_json::Value`; structs become objects keyed by field.

use std::fmt::Write;
use std::path::Path;

#[derive(Debug, Clone)]
enum Ty {
    Bool,
    Int,
    Float,
    Str,
    Image,
    Struct(String),
    List(Box<Ty>),
}

fn ty(s: &str) -> Ty {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return Ty::List(Box::new(ty(inner)));
    }
    match s {
        "bool" => Ty::Bool,
        "int" => Ty::Int,
        "float" => Ty::Float,
        "string" => Ty::Str,
        "image" => Ty::Image,
        other => Ty::Struct(other.to_string()),
    }
}

fn snake(s: &str) -> String {
    s.replace('-', "_")
}

/// Rust expression turning `v` (a value of type `t`) into a `Value`.
fn to_json(t: &Ty, v: &str) -> String {
    match t {
        Ty::Bool | Ty::Int | Ty::Float => format!("Value::from({v})"),
        Ty::Str => format!("Value::from({v}.as_str())"),
        Ty::Image => "Value::Null".into(),
        Ty::Struct(n) => format!("{}_to_json(&{v})", snake(&n.to_lowercase())),
        Ty::List(inner) => format!("Value::Array({v}.iter().map(|x| {}).collect())", to_json(inner, "x")),
    }
}

/// Rust expression turning `v` (a `&Value`) into type `t`.
fn from_json(t: &Ty, v: &str) -> String {
    match t {
        Ty::Bool => format!("{v}.as_bool().unwrap_or_default()"),
        Ty::Int => format!("({v}.as_i64().unwrap_or_default() as i32)"),
        Ty::Float => format!("({v}.as_f64().unwrap_or_default() as f32)"),
        Ty::Str => format!("SharedString::from({v}.as_str().unwrap_or_default())"),
        Ty::Image => "slint::Image::default()".into(),
        Ty::Struct(n) => format!("{}_from_json({v})", snake(&n.to_lowercase())),
        Ty::List(inner) => format!(
            "ModelRc::new(VecModel::from({v}.as_array().map(|a| a.iter().map(|x| {}).collect::<Vec<_>>()).unwrap_or_default()))",
            from_json(inner, "x")
        ),
    }
}

/// Writes a list into the property's existing `VecModel`: only rows that
/// differ change, so the repeater keeps the others (and any press on them).
const SYNC_LIST: &str = r#"#[allow(dead_code)]
fn sync_list<T: Clone + PartialEq + 'static>(current: ModelRc<T>, rows: Vec<T>, set: impl FnOnce(ModelRc<T>)) {
    let Some(vm) = current.as_any().downcast_ref::<VecModel<T>>() else {
        set(ModelRc::new(VecModel::from(rows)));
        return;
    };
    if vm.row_count() != rows.len() {
        vm.set_vec(rows);
        return;
    }
    for (i, r) in rows.into_iter().enumerate() {
        if vm.row_data(i).as_ref() != Some(&r) {
            vm.set_row_data(i, r);
        }
    }
}

"#;

fn block<'a>(src: &'a str, head: &str) -> &'a str {
    let start = src.find(head).unwrap_or_else(|| panic!("{head} not found")) + head.len();
    let end = src[start..].find("\n}").map(|e| start + e).unwrap_or(src.len());
    &src[start..end]
}

pub fn generate(state: &Path, out: &Path) {
    let src = std::fs::read_to_string(state).expect("read state.slint");
    let mut structs = Vec::new();
    for line in src.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("export struct ") {
            let (name, body) = rest.split_once('{').expect("struct body");
            let body = body.trim().trim_end_matches('}');
            let fields: Vec<(String, Ty)> = body
                .split(',')
                .filter_map(|f| f.split_once(':'))
                .map(|(n, t)| (n.trim().to_string(), ty(t)))
                .collect();
            structs.push((name.trim().to_string(), fields));
        }
    }
    let mut props = Vec::new();
    for line in block(&src, "export global Live {").lines() {
        let l = line.trim();
        let rest = l.strip_prefix("in-out property <").or_else(|| l.strip_prefix("in property <"));
        let Some(rest) = rest else { continue };
        let (t, rest) = rest.split_once('>').expect("property type");
        let name = rest.trim().split([':', ';']).next().unwrap().trim().to_string();
        props.push((name, ty(t)));
    }
    let mut callbacks = Vec::new();
    for line in block(&src, "export global Actions {").lines() {
        let Some(rest) = line.trim().strip_prefix("callback ") else { continue };
        let (name, args) = rest.split_once('(').expect("callback args");
        let args: Vec<Ty> = args
            .split(')')
            .next()
            .unwrap()
            .split(',')
            .filter_map(|a| a.split_once(':'))
            .map(|(_, t)| ty(t))
            .collect();
        callbacks.push((name.trim().to_string(), args));
    }

    let mut o = String::new();
    o.push_str("// Generated by uimirror from state.slint. Do not edit.\n");
    o.push_str("#[allow(unused_imports)]\nuse slint::{Model, ModelRc, SharedString, VecModel};\nuse serde_json::Value;\n\n");
    o.push_str(SYNC_LIST);
    for (name, fields) in &structs {
        let f = snake(&name.to_lowercase());
        let _ = writeln!(o, "#[allow(dead_code)]\nfn {f}_to_json(v: &{name}) -> Value {{\n    let mut m = serde_json::Map::new();");
        for (n, t) in fields {
            let _ = writeln!(o, "    m.insert({n:?}.into(), {});", to_json(t, &format!("v.{}", snake(n))));
        }
        let _ = writeln!(o, "    Value::Object(m)\n}}\n");
        let _ = writeln!(o, "#[allow(dead_code)]\nfn {f}_from_json(v: &Value) -> {name} {{\n    {name} {{");
        for (n, t) in fields {
            let _ = writeln!(o, "        {}: {},", snake(n), from_json(t, &format!("v[{n:?}]")));
        }
        let _ = writeln!(o, "    }}\n}}\n");
    }
    let mirrored: Vec<_> = props.iter().filter(|(_, t)| !matches!(t, Ty::Image)).collect();
    let _ = writeln!(o, "#[allow(dead_code)]\npub const PROPS: &[&str] = &[{}];\n", mirrored.iter().map(|(n, _)| format!("{n:?}")).collect::<Vec<_>>().join(", "));
    let _ = writeln!(o, "#[allow(dead_code)]\npub fn get_prop(live: &Live, name: &str) -> Option<Value> {{\n    Some(match name {{");
    for (n, t) in &mirrored {
        let _ = writeln!(o, "        {n:?} => {{ let v = live.get_{}(); {} }}", snake(n), to_json(t, "v"));
    }
    let _ = writeln!(o, "        _ => return None,\n    }})\n}}\n");
    let _ = writeln!(o, "#[allow(dead_code)]\npub fn set_prop(live: &Live, name: &str, v: &Value) {{\n    match name {{");
    for (n, t) in &mirrored {
        // A list updates the model it already has, row by row, as the app's
        // own `models::sync` does: a new model rebuilds every row, which
        // drops a press or drag on one of them.
        if let Ty::List(inner) = t {
            let rows = format!("{v}.as_array().map(|a| a.iter().map(|x| {}).collect::<Vec<_>>()).unwrap_or_default()", from_json(inner, "x"), v = "v");
            let _ = writeln!(o, "        {n:?} => sync_list(live.get_{0}(), {rows}, |m| live.set_{0}(m)),", snake(n));
        } else {
            let _ = writeln!(o, "        {n:?} => live.set_{}({}),", snake(n), from_json(t, "v"));
        }
    }
    let _ = writeln!(o, "        _ => {{}}\n    }}\n}}\n");
    let _ = writeln!(o, "#[allow(dead_code)]\npub fn invoke_action(actions: &Actions, name: &str, args: &[Value]) -> bool {{\n    let null = Value::Null;\n    let _arg = |i: usize| args.get(i).unwrap_or(&null);\n    match name {{");
    for (n, args) in &callbacks {
        let a: Vec<String> = args.iter().enumerate().map(|(i, t)| from_json(t, &format!("_arg({i})"))).collect();
        let _ = writeln!(o, "        {n:?} => actions.invoke_{}({}),", snake(n), a.join(", "));
    }
    let _ = writeln!(o, "        _ => return false,\n    }}\n    true\n}}\n");
    let _ = writeln!(o, "#[allow(dead_code)]\npub fn forward_actions(actions: &Actions, f: std::rc::Rc<dyn Fn(&str, Vec<Value>)>, skip: &[&str]) {{");
    for (n, args) in &callbacks {
        let names: Vec<String> = (0..args.len()).map(|i| format!("a{i}")).collect();
        let vals: Vec<String> = args.iter().enumerate().map(|(i, t)| to_json(t, &format!("a{i}"))).collect();
        let _ = writeln!(
            o,
            "    if !skip.contains(&{n:?}) {{ let f = f.clone(); actions.on_{}(move |{}| f({n:?}, vec![{}])); }}",
            snake(n),
            names.join(", "),
            vals.join(", ")
        );
    }
    o.push_str("}\n");
    std::fs::write(out, o).expect("write mirror.rs");
}
