//! Replays the recorded request sequences in `tests/fixtures` through the
//! Rust `Helper` and compares every reply, the files after every request,
//! and the final file contents with what the Python helper produced
//! (`record_fixtures.py` records them).

use jobcore::py::{dumps_compact, loads, sha256_hex};
use jobcore::{Helper, Hooks};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn digest(files: &BTreeMap<String, Vec<u8>>, tmp: &str) -> String {
    let mut data = Vec::new();
    for (path, content) in files {
        data.extend_from_slice(path.as_bytes());
        data.push(0);
        // Paths inside files (.last-job) name the temporary folder.
        let text = String::from_utf8_lossy(content).replace(tmp, "$TMP");
        data.extend_from_slice(text.as_bytes());
        data.push(0);
    }
    sha256_hex(&data)
}

struct Context {
    tmp: String,
    load: String,
    ops: Vec<String>,
}

impl Context {
    fn program_lines(&self) -> Vec<String> {
        let text = std::fs::read_to_string(Path::new(&self.tmp).join(&self.load)).unwrap();
        text.split('\n').map(|s| s.to_string()).collect()
    }

    fn sub(&self, v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), self.sub(v))).collect()),
            Value::Array(a) => Value::Array(a.iter().map(|v| self.sub(v)).collect()),
            Value::String(s) => {
                if let Some(rest) = s.strip_prefix("$OPLINE") {
                    let (n, k) = match rest.find(['+', '-']) {
                        Some(i) => (&rest[..i], rest[i..].trim_start_matches('+').parse::<i64>().unwrap()),
                        None => (rest, 0),
                    };
                    let marker = format!("(MYMILL-OP {}/", n);
                    let first = self.program_lines().iter().position(|l| l.starts_with(&marker)).unwrap() as i64 + 1;
                    return Value::from(first + k);
                }
                if let Some(n) = s.strip_prefix("$STOPLINE") {
                    let marker = format!("o<job_op{}_stop> endif", n);
                    let at = self.program_lines().iter().position(|l| *l == marker).unwrap() as i64 + 1;
                    return Value::from(at - 1);
                }
                if let Some(n) = s.strip_prefix("$OP") {
                    if let Ok(i) = n.parse::<usize>() {
                        return Value::String(self.ops[i - 1].clone());
                    }
                }
                Value::String(s.replace("$TMP", &self.tmp).replace("$LOAD", &self.load))
            }
            other => other.clone(),
        }
    }

    fn note(&mut self, reply: &Value) {
        if let Some(Value::String(l)) = reply.get("load") {
            if !l.is_empty() {
                self.load = l.clone();
            }
        }
        if let Some(Value::Array(ops)) = reply.get("snap").and_then(|s| s.get("ops")) {
            self.ops = ops.iter().map(|o| o["id"].as_str().unwrap().to_string()).collect();
        }
    }

    fn path(&self, v: &Value) -> PathBuf {
        Path::new(&self.tmp).join(self.sub(v).as_str().unwrap())
    }

    fn action(&self, entry: &serde_json::Map<String, Value>) {
        if let Some(p) = entry.get("_write") {
            std::fs::write(self.path(p), self.sub(&entry["text"]).as_str().unwrap()).unwrap();
        } else if let Some(p) = entry.get("_replace") {
            let text = std::fs::read_to_string(self.path(p)).unwrap();
            let old = self.sub(&entry["old"]);
            let new = self.sub(&entry["new"]);
            std::fs::write(self.path(p), text.replace(old.as_str().unwrap(), new.as_str().unwrap())).unwrap();
        } else if let Some(p) = entry.get("_copy") {
            std::fs::copy(self.path(p), self.path(&entry["to"])).unwrap();
        } else if let Some(p) = entry.get("_remove") {
            std::fs::remove_file(self.path(p)).unwrap();
        } else {
            panic!("unknown harness action {:?}", entry);
        }
    }
}

fn hooks() -> Hooks {
    let counter = Arc::new(Mutex::new(0u32));
    Hooks {
        new_id: Box::new(move || {
            let mut c = counter.lock().unwrap();
            *c += 1;
            format!("{:08x}", *c)
        }),
        job_name: Box::new(|| "JOB 01-02 0304".to_string()),
    }
}

fn run_fixture(name: &str) -> Vec<String> {
    static N: AtomicU64 = AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!("jobcore-{}-{}-{}", std::process::id(), name, N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let tmp = std::fs::canonicalize(&tmp).unwrap();
    std::fs::copy(fixtures().join("tool.tbl"), tmp.join("tool.tbl")).unwrap();
    let tmp_s = tmp.to_string_lossy().into_owned();

    let requests = std::fs::read_to_string(fixtures().join(format!("{}.jsonl", name))).unwrap();
    let expected = std::fs::read_to_string(fixtures().join(format!("{}.expected.jsonl", name))).unwrap();
    let mut expected: Vec<Value> = expected.lines().map(|l| loads(l).unwrap()).collect();
    let final_files = expected.pop().unwrap();

    let mut helper = Helper::with_hooks(tmp.clone(), hooks());
    let mut ctx = Context { tmp: tmp_s.clone(), load: String::new(), ops: vec![] };
    let mut problems = Vec::new();
    let mut index = 0;
    for line in requests.lines() {
        if line.trim().is_empty() || line.starts_with("//") {
            continue;
        }
        let entry = loads(line).unwrap();
        if let Value::Object(m) = &entry {
            if m.keys().any(|k| k.starts_with('_')) {
                ctx.action(m);
                continue;
            }
        }
        let req = ctx.sub(&entry);
        let reply = helper.handle(&req);
        ctx.note(&reply);
        let want = &expected[index];
        let want_reply = loads(want["reply"].as_str().unwrap()).unwrap();
        let got = loads(&dumps_compact(&reply).replace(&tmp_s, "$TMP")).unwrap();
        if got != want_reply {
            problems.push(format!(
                "{} request {} {}\n  python: {}\n  rust:   {}",
                name,
                index + 1,
                line,
                dumps_compact(&want_reply),
                dumps_compact(&got)
            ));
        }
        let files = tree(&tmp);
        if digest(&files, &tmp_s) != want["tree"].as_str().unwrap() {
            problems.push(format!("{} request {} {}: files differ: {:?}", name, index + 1, line, files.keys().collect::<Vec<_>>()));
        }
        index += 1;
    }
    assert_eq!(index, expected.len(), "{}: request count", name);

    let got: BTreeMap<String, String> = tree(&tmp)
        .into_iter()
        .map(|(k, v)| (k, String::from_utf8(v).unwrap().replace(&tmp_s, "$TMP")))
        .collect();
    let want: BTreeMap<String, String> = final_files["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    if got.keys().collect::<Vec<_>>() != want.keys().collect::<Vec<_>>() {
        problems.push(format!("{}: final files {:?} != {:?}", name, got.keys(), want.keys()));
    }
    for (k, v) in &want {
        if got.get(k) != Some(v) {
            problems.push(format!("{}: {} differs\n--- python\n{}\n--- rust\n{}", name, k, v, got.get(k).cloned().unwrap_or_default()));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    problems
}

#[test]
fn replies_and_files_match_the_python_helper() {
    let mut names: Vec<String> = std::fs::read_dir(fixtures())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jsonl") && !n.ends_with(".expected.jsonl"))
        .map(|n| n.trim_end_matches(".jsonl").to_string())
        .collect();
    names.sort();
    assert!(!names.is_empty());
    let mut problems = Vec::new();
    for name in &names {
        problems.extend(run_fixture(name));
    }
    if !problems.is_empty() {
        let shown: Vec<&String> = problems.iter().take(15).collect();
        for p in &shown {
            eprintln!("{}\n", p);
        }
        panic!("{} mismatches with the Python helper", problems.len());
    }
}
