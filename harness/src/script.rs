//! Scenario files (`scenarios/*.scn`): one step per line.
//!
//! ```text
//! title   Home and jog                # shown in the report
//! needs   probe-basic                 # skip on configs without every tag
//! xfail   inch: B3, inch not handled  # expected to fail on a config or tag
//! allow-error  Probe tripped          # LinuxCNC/millgui error text that is fine
//! include common/ready.scn            # another file's steps, here
//!
//! do      jog-start 0 1               # an Actions callback; args are JSON or bare words
//! set     mdi-text "G0 X1"            # a two-way Live property the remote may set
//! mdi     G0 X${safe_x}               # mdi-send, then wait for the machine to settle
//! idle    120                         # wait (seconds) for the machine to settle
//! tap     files name h_square.ngc files-tap   # call an action with a list row's index
//! screen  probe                       # navigate, wait until it shows
//! wait    stat.homed[0] == 1 within 60
//! expect  prop.tool == "T3"
//! hal     hm2_7i76e.0.sim.force-probe 1
//! sleep   0.5
//! shot    after-jog                   # screenshot into the report
//! golden  after-jog 0.5               # screenshot compared with goldens/<config>/
//! log     any text for the report
//! ```
//!
//! `${name}` is replaced from the config's `vars` file before parsing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum Step {
    Do {
        action: String,
        args: Vec<serde_json::Value>,
    },
    Set {
        prop: String,
        value: serde_json::Value,
    },
    Mdi(String),
    /// Wait until the interpreter is idle, the queue empty and motion in position.
    Idle(f64),
    /// Find the row of a list property whose field equals a value, and call
    /// an action with its index: `tap files name square.ngc files-tap`.
    Tap {
        list: String,
        field: String,
        value: String,
        action: String,
    },
    Screen(String),
    Wait {
        expr: String,
        within: f64,
    },
    Expect(String),
    Hal {
        pin: String,
        value: String,
    },
    Sleep(f64),
    Shot(String),
    Golden {
        name: String,
        max_pct: f64,
    },
    Log(String),
}

#[derive(Debug, Clone)]
pub struct Line {
    pub file: String,
    pub no: usize,
    pub text: String,
    pub step: Step,
}

#[derive(Debug, Default)]
pub struct Scenario {
    pub name: String,
    pub title: String,
    pub needs: Vec<String>,
    /// (config name or tag, reason)
    pub xfail: Vec<(String, String)>,
    pub allow_errors: Vec<String>,
    pub steps: Vec<Line>,
}

impl Scenario {
    pub fn load(
        path: &Path,
        root: &Path,
        vars: &HashMap<String, String>,
    ) -> Result<Scenario, String> {
        let name = path
            .strip_prefix(root)
            .unwrap_or(path)
            .with_extension("")
            .to_string_lossy()
            .to_string();
        let mut sc = Scenario {
            name: name.clone(),
            title: name,
            ..Default::default()
        };
        sc.read(path, root, vars, 0)?;
        Ok(sc)
    }

    fn read(
        &mut self,
        path: &Path,
        root: &Path,
        vars: &HashMap<String, String>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 8 {
            return Err(format!("{}: include nested too deep", path.display()));
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        for (i, raw) in text.lines().enumerate() {
            let no = i + 1;
            let line = strip_comment(raw).trim().to_string();
            if line.is_empty() {
                continue;
            }
            let line = substitute(&line, vars).map_err(|e| format!("{file}:{no}: {e}"))?;
            let (word, rest) = split_word(&line);
            let err = |m: &str| format!("{file}:{no}: {m}: {line}");
            let step = match word {
                "title" => {
                    if depth == 0 {
                        self.title = rest.to_string();
                    }
                    continue;
                }
                "needs" => {
                    self.needs.extend(rest.split_whitespace().map(String::from));
                    continue;
                }
                "xfail" => {
                    let (who, why) = rest.split_once(':').unwrap_or((rest, ""));
                    for w in who.split_whitespace() {
                        self.xfail.push((w.to_string(), why.trim().to_string()));
                    }
                    continue;
                }
                "allow-error" => {
                    self.allow_errors.push(rest.to_string());
                    continue;
                }
                "include" => {
                    let p = resolve(root, path, rest);
                    self.read(&p, root, vars, depth + 1)?;
                    continue;
                }
                "do" => {
                    let toks = tokens(rest).map_err(|e| err(&e))?;
                    let Some((action, args)) = toks.split_first() else {
                        return Err(err("do needs an action"));
                    };
                    Step::Do {
                        action: action.as_str().unwrap_or_default().to_string(),
                        args: args.to_vec(),
                    }
                }
                "set" => {
                    let (prop, v) = split_word(rest);
                    Step::Set {
                        prop: prop.to_string(),
                        value: value(v),
                    }
                }
                "mdi" => Step::Mdi(rest.to_string()),
                "idle" => Step::Idle(if rest.is_empty() {
                    60.0
                } else {
                    rest.parse().map_err(|_| err("bad seconds"))?
                }),
                "tap" => {
                    let t: Vec<&str> = rest.split_whitespace().collect();
                    let [list, field, value, action] = t[..] else {
                        return Err(err("tap LIST FIELD VALUE ACTION"));
                    };
                    Step::Tap {
                        list: list.into(),
                        field: field.into(),
                        value: value.into(),
                        action: action.into(),
                    }
                }
                "screen" => Step::Screen(rest.to_string()),
                "wait" => {
                    let (expr, within) = match rest.rsplit_once(" within ") {
                        Some((e, w)) => (
                            e.trim(),
                            w.trim()
                                .parse::<f64>()
                                .map_err(|_| err("bad 'within' seconds"))?,
                        ),
                        None => (rest, 30.0),
                    };
                    Step::Wait {
                        expr: expr.to_string(),
                        within,
                    }
                }
                "expect" => Step::Expect(rest.to_string()),
                "hal" => {
                    let (pin, v) = split_word(rest);
                    Step::Hal {
                        pin: pin.to_string(),
                        value: v.to_string(),
                    }
                }
                "sleep" => Step::Sleep(rest.parse().map_err(|_| err("bad seconds"))?),
                "shot" => Step::Shot(rest.to_string()),
                "golden" => {
                    let (n, pct) = split_word(rest);
                    let max_pct = if pct.is_empty() {
                        0.5
                    } else {
                        pct.parse().map_err(|_| err("bad percent"))?
                    };
                    Step::Golden {
                        name: n.to_string(),
                        max_pct,
                    }
                }
                "log" => Step::Log(rest.to_string()),
                _ => return Err(err("unknown step")),
            };
            self.steps.push(Line {
                file: file.clone(),
                no,
                text: line.clone(),
                step,
            });
        }
        Ok(())
    }
}

fn resolve(root: &Path, from: &Path, name: &str) -> PathBuf {
    let rel = from.parent().unwrap_or(root).join(name);
    if rel.exists() {
        rel
    } else {
        root.join(name)
    }
}

/// `#` starts a comment unless it is inside quotes or part of a word
/// (G-code parameters like `#<_x>` or `#5221` stay).
fn strip_comment(s: &str) -> &str {
    let mut quoted = false;
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'"' => quoted = !quoted,
            b'#' if !quoted
                && (i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t')
                && b.get(i + 1).is_none_or(|n| *n == b' ' || *n == b'\t') =>
            {
                return &s[..i]
            }
            _ => {}
        }
    }
    s
}

pub fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim()),
        None => (s, ""),
    }
}

fn substitute(s: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let j = after.find('}').ok_or("unclosed ${")?;
        let key = &after[..j];
        let v = vars
            .get(key)
            .ok_or_else(|| format!("config has no var '{key}'"))?;
        out.push_str(v);
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A bare word or JSON value.
pub fn value(s: &str) -> serde_json::Value {
    let s = s.trim();
    serde_json::from_str(s).unwrap_or_else(|_| serde_json::Value::String(s.to_string()))
}

/// Whitespace-separated values; "quoted strings" and [json arrays] stay whole.
fn tokens(s: &str) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    let mut it = s.trim().char_indices().peekable();
    let s = s.trim();
    while let Some(&(start, c)) = it.peek() {
        if c.is_whitespace() {
            it.next();
            continue;
        }
        let end = if c == '"' {
            it.next();
            let mut esc = false;
            let mut end = None;
            for (i, ch) in it.by_ref() {
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == '"' {
                    end = Some(i + 1);
                    break;
                }
            }
            end.ok_or("unclosed quote")?
        } else {
            let mut end = s.len();
            for (i, ch) in it.by_ref() {
                if ch.is_whitespace() {
                    end = i;
                    break;
                }
            }
            end
        };
        out.push(value(&s[start..end]));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steps_and_vars() {
        let dir = std::env::temp_dir().join(format!("harness-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.scn");
        std::fs::write(
            &p,
            "title Demo\nneeds probe-basic\nxfail inch sim-xyza: not yet\n# comment\ndo jog-start 0 1\ndo mdi-send \"G0 X1\"\nmdi G0 X${x} # go\nmdi #<_x> = 1\nwait stat.homed[0] == 1 within 5\n",
        )
        .unwrap();
        let vars = HashMap::from([("x".to_string(), "12.5".to_string())]);
        let sc = Scenario::load(&p, &dir, &vars).unwrap();
        assert_eq!(sc.title, "Demo");
        assert_eq!(sc.needs, vec!["probe-basic"]);
        assert_eq!(sc.xfail.len(), 2);
        assert!(
            matches!(&sc.steps[0].step, Step::Do { action, args } if action == "jog-start" && args == &vec![serde_json::json!(0), serde_json::json!(1)])
        );
        assert!(
            matches!(&sc.steps[1].step, Step::Do { args, .. } if args == &vec![serde_json::json!("G0 X1")])
        );
        assert!(matches!(&sc.steps[2].step, Step::Mdi(t) if t == "G0 X12.5"));
        assert!(matches!(&sc.steps[3].step, Step::Mdi(t) if t == "#<_x> = 1"));
        assert!(matches!(&sc.steps[4].step, Step::Wait { within, .. } if *within == 5.0));
        std::fs::remove_dir_all(dir).ok();
    }
}
