//! One simulator run: LinuxCNC with a prepared config and ferrum as its
//! display on a private X server, a websocket to ferrum, and a LinuxCNC
//! status helper. Dropping it shuts everything down.

use serde_json::{Map, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};

pub struct Env {
    pub lcnc: PathBuf,
    pub ferrum_bin: PathBuf,
    pub display: String,
    pub port: u16,
    pub harness_dir: PathBuf,
}

pub struct Session {
    child: Option<Child>,
    ws_out: Arc<Mutex<Option<WebSocket<MaybeTlsStream<TcpStream>>>>>,
    pub props: Arc<Mutex<Map<String, Value>>>,
    /// Every ferrum message row seen with kind "error".
    pub ui_errors: Arc<Mutex<Vec<String>>>,
    helper: Option<(Child, ChildStdin, BufReader<ChildStdout>)>,
    rip: PathBuf,
    display: String,
    port: u16,
    pub log: PathBuf,
    cfg_dir: PathBuf,
    home: PathBuf,
    helper_py: PathBuf,
}

fn rip_cmd(lcnc: &Path, script: &str) -> Command {
    let mut c = Command::new("bash");
    c.arg("-c").arg(format!(
        "set +u; source '{}/scripts/rip-environment' >/dev/null; {script}",
        lcnc.display()
    ));
    c
}

impl Session {
    pub fn start(env: &Env, cfg_dir: &Path, log: PathBuf) -> Result<Session, String> {
        cleanup(&env.lcnc);
        let out = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        // A fresh HOME per run: ferrum keeps settings and the probe's
        // restore-pending marker there, LinuxCNC its tool mmap.
        let home = cfg_dir.parent().unwrap_or(cfg_dir).join("home");
        std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        // Start the way the Pi does (ferrum/boot): `ferrum --boot`, the
        // splash launcher, which runs LinuxCNC itself. HARNESS_BOOT=0 starts
        // `linuxcnc` with ferrum as its display instead.
        let start = if std::env::var("HARNESS_BOOT").is_ok_and(|v| v == "0") {
            "exec linuxcnc ferrum.ini".to_string()
        } else {
            format!("exec '{}' --boot ferrum.ini", env.ferrum_bin.display())
        };
        let mut cmd = rip_cmd(&env.lcnc, &start);
        cmd.current_dir(cfg_dir)
            .env("HOME", &home)
            .env("DISPLAY", &env.display)
            .env("FERRUM_BIN", &env.ferrum_bin)
            .env("FERRUM_HARNESS", "1")
            .env("FERRUM_RENDERER", std::env::var("HARNESS_RENDERER").unwrap_or_else(|_| "software".into()))
            .env("FERRUM_LOG_MESSAGES", "1")
            .env("LD_LIBRARY_PATH", env.lcnc.join("lib"))
            .stdin(Stdio::null())
            .stdout(out.try_clone().map_err(|e| e.to_string())?)
            .stderr(out);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn().map_err(|e| format!("linuxcnc: {e}"))?;
        let mut s = Session {
            child: Some(child),
            ws_out: Arc::new(Mutex::new(None)),
            props: Arc::new(Mutex::new(Map::new())),
            ui_errors: Arc::new(Mutex::new(Vec::new())),
            helper: None,
            rip: env.lcnc.clone(),
            display: env.display.clone(),
            port: env.port,
            log,
            cfg_dir: cfg_dir.to_path_buf(),
            home: home.clone(),
            helper_py: env.harness_dir.join("lcnc_helper.py"),
        };
        s.connect(env.port, Duration::from_secs(90))?;
        // Measure frame gaps from here: the splash's hand-over to the main
        // screen is checked by the HAL watchdog itself.
        s.action("failsafe-reset-gap", &[])?;
        s.start_helper()?;
        Ok(s)
    }

    /// The status helper (lcnc_helper.py), connected to LinuxCNC.
    fn start_helper(&mut self) -> Result<(), String> {
        let mut h = rip_cmd(&self.rip, &format!("exec python3 -u '{}'", self.helper_py.display()))
            .current_dir(&self.cfg_dir)
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("lcnc_helper: {e}"))?;
        let stdin = h.stdin.take().unwrap();
        let stdout = BufReader::new(h.stdout.take().unwrap());
        self.helper = Some((h, stdin, stdout));
        Ok(())
    }

    /// Ferrum restarted LinuxCNC (SETUP's RESTART LINUXCNC): wait for its
    /// websocket to close, then connect again, with a new status helper.
    pub fn reconnect(&mut self, within: Duration) -> Result<(), String> {
        let t0 = Instant::now();
        while self.ws_out.lock().unwrap().is_some() {
            if t0.elapsed() > within {
                return Err("ferrum's websocket did not close".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some((mut h, _, _)) = self.helper.take() {
            let _ = h.kill();
            let _ = h.wait();
        }
        self.props.lock().unwrap().clear();
        let left = within.saturating_sub(t0.elapsed()).max(Duration::from_secs(30));
        self.connect(self.port, left)?;
        self.action("failsafe-reset-gap", &[])?;
        self.start_helper()
    }


    fn connect(&mut self, port: u16, within: Duration) -> Result<(), String> {
        let t0 = Instant::now();
        let ws = loop {
            if let Some(c) = self.child.as_mut() {
                if let Ok(Some(st)) = c.try_wait() {
                    return Err(format!(
                        "LinuxCNC exited ({st}) before ferrum answered; see {}",
                        self.log.display()
                    ));
                }
            }
            match tungstenite::connect(format!("ws://127.0.0.1:{port}/")) {
                Ok((ws, _)) => break ws,
                Err(_) if t0.elapsed() < within => std::thread::sleep(Duration::from_millis(250)),
                Err(e) => {
                    return Err(format!(
                        "ferrum websocket on {port}: {e} (see {})",
                        self.log.display()
                    ))
                }
            }
        };
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            let _ = s.set_read_timeout(Some(Duration::from_millis(30)));
        }
        *self.ws_out.lock().unwrap() = Some(ws);
        // One thread owns reads and writes on the socket (tungstenite is not
        // split); senders queue through `ws_out` between reads.
        let (wso, props, errs) = (
            self.ws_out.clone(),
            self.props.clone(),
            self.ui_errors.clone(),
        );
        std::thread::spawn(move || loop {
            let msg = {
                let mut g = wso.lock().unwrap();
                let Some(ws) = g.as_mut() else { return };
                match ws.read() {
                    Ok(m) => Some(m),
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        None
                    }
                    Err(_) => {
                        *g = None;
                        return;
                    }
                }
            };
            match msg {
                Some(Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                        if let Some(p) = v.get("p").and_then(|p| p.as_object()) {
                            if let Some(rows) = p.get("messages").and_then(|m| m.as_array()) {
                                let mut e = errs.lock().unwrap();
                                for r in rows {
                                    if r["kind"] == "error" {
                                        let text = format!(
                                            "{} {}",
                                            r["time"].as_str().unwrap_or(""),
                                            r["text"].as_str().unwrap_or("")
                                        );
                                        if !e.contains(&text) {
                                            e.push(text);
                                        }
                                    }
                                }
                            }
                            props.lock().unwrap().extend(p.clone());
                        }
                    }
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        });
        // The first full property set arrives within a tick or two.
        let t0 = Instant::now();
        while self.props.lock().unwrap().is_empty() {
            if t0.elapsed() > Duration::from_secs(10) {
                return Err("ferrum sent no properties".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    fn send(&self, v: Value) -> Result<(), String> {
        let mut g = self.ws_out.lock().unwrap();
        let ws = g.as_mut().ok_or("ferrum websocket closed")?;
        ws.send(Message::text(v.to_string()))
            .map_err(|e| format!("websocket: {e}"))
    }

    pub fn action(&self, name: &str, args: &[Value]) -> Result<(), String> {
        self.send(serde_json::json!({ "a": name, "args": args }))
    }

    pub fn set_prop(&self, name: &str, v: &Value) -> Result<(), String> {
        self.send(serde_json::json!({ "s": { name: v } }))
    }

    pub fn prop_snapshot(&self) -> Value {
        Value::Object(self.props.lock().unwrap().clone())
    }

    fn ask(&mut self, cmd: &str) -> Result<Value, String> {
        let (_, stdin, stdout) = self.helper.as_mut().ok_or("no helper")?;
        writeln!(stdin, "{cmd}").map_err(|e| format!("lcnc_helper: {e}"))?;
        let mut line = String::new();
        stdout
            .read_line(&mut line)
            .map_err(|e| format!("lcnc_helper: {e}"))?;
        let v: Value =
            serde_json::from_str(&line).map_err(|e| format!("lcnc_helper said {line:?}: {e}"))?;
        if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
            return Err(format!("lcnc_helper: {e}"));
        }
        Ok(v)
    }

    /// Stop the helper reading LinuxCNC's error queue: ferrum gets every error.
    pub fn errors_to_ferrum(&mut self) -> Result<(), String> {
        self.ask("nodrain").map(|_| ())
    }

    pub fn stat(&mut self) -> Result<Value, String> {
        self.ask("stat")
    }

    /// Error-channel messages so far, as texts (only kind "error").
    pub fn errors(&mut self) -> Result<Vec<String>, String> {
        let v = self.ask("errors")?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter(|m| m["kind"] == "error")
                    .filter_map(|m| m["text"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn hal_get(&self, name: &str) -> Result<Value, String> {
        let out = rip_cmd(
            &self.rip,
            &format!("halcmd -s getp '{name}' 2>/dev/null || halcmd -s gets '{name}'"),
        )
        .output()
        .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("no HAL pin or signal '{name}'"));
        }
        Ok(Value::String(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    }

    pub fn hal_set(&self, name: &str, v: &str) -> Result<(), String> {
        let out = rip_cmd(
            &self.rip,
            &format!("halcmd setp '{name}' '{v}' 2>/dev/null || halcmd sets '{name}' '{v}'"),
        )
        .output()
        .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "halcmd could not set '{name}': {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    pub fn screenshot(&self, path: &Path) -> Result<(), String> {
        let out = Command::new("import")
            .args([
                "-display",
                &self.display,
                "-window",
                "root",
                &path.to_string_lossy(),
            ])
            .output()
            .map_err(|e| format!("import (ImageMagick): {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    /// ferrum's GUI process: under `--boot` the one with that flag
    /// (LinuxCNC's own display copy only waits), else the only one.
    fn gui_pid() -> Result<String, String> {
        for args in [["-n", "-f", "ferrum --boot"], ["-n", "-x", "ferrum"]] {
            let o = Command::new("pgrep").args(args).output().map_err(|e| e.to_string())?;
            let pid = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !pid.is_empty() {
                return Ok(pid);
            }
        }
        Err("no ferrum process".into())
    }

    pub fn signal_gui(&mut self, sig: &str) -> Result<(), String> {
        let pid = Self::gui_pid()?;
        let st = Command::new("kill").args([&format!("-{sig}"), &pid]).status().map_err(|e| e.to_string())?;
        if !st.success() {
            return Err(format!("kill -{sig} {pid} failed"));
        }
        // A stopped ferrum may drop the websocket: connect again.
        if sig == "CONT" {
            std::thread::sleep(Duration::from_millis(500));
            if self.ws_out.lock().unwrap().is_none() {
                self.connect(self.port, Duration::from_secs(10))?;
            }
        }
        Ok(())
    }

    pub fn click(&self, x: i32, y: i32) -> Result<(), String> {
        let out = Command::new("xdotool")
            .env("DISPLAY", &self.display)
            .args(["mousemove", &x.to_string(), &y.to_string(), "click", "1"])
            .output()
            .map_err(|e| format!("xdotool: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    /// Press at `from`, move to `to` in small steps as a finger would, let go.
    pub fn drag(&self, from: (i32, i32), to: (i32, i32)) -> Result<(), String> {
        let mut args = vec!["mousemove".to_string(), from.0.to_string(), from.1.to_string(), "mousedown".into(), "1".into()];
        const STEPS: i32 = 20;
        for i in 1..=STEPS {
            let x = from.0 + (to.0 - from.0) * i / STEPS;
            let y = from.1 + (to.1 - from.1) * i / STEPS;
            args.extend(["sleep".into(), "0.02".into(), "mousemove".into(), x.to_string(), y.to_string()]);
        }
        args.extend(["sleep".into(), "0.05".into(), "mouseup".into(), "1".into()]);
        let out = Command::new("xdotool")
            .env("DISPLAY", &self.display)
            .args(&args)
            .output()
            .map_err(|e| format!("xdotool: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    pub fn alive(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }
}

/// What a previous run may have left: LinuxCNC's processes and shared memory.
pub fn cleanup(lcnc: &Path) {
    for p in [
        "ferrum",
        "milltask",
        "halui",
        "io",
        "iov2",
        "rtapi_app",
        "linuxcncsvr",
        "sim_card.py",
        "sim_pendant.py",
    ] {
        let _ = Command::new("pkill").args(["-9", "-x", p]).status();
    }
    let _ = Command::new("pkill")
        .args(["-9", "-f", "sim_card.py|sim_pendant.py|lcnc_helper.py"])
        .status();
    let _ = rip_cmd(lcnc, "halrun -U >/dev/null 2>&1").status();
    // A killed session leaves its lock; `ferrum --boot` would ask about it.
    let _ = std::fs::remove_file("/tmp/linuxcnc.lock");
}

impl Drop for Session {
    fn drop(&mut self) {
        // A scenario that failed with ferrum stopped (signal STOP) must not
        // leave it stopped: it could not exit.
        let _ = Command::new("pkill").args(["-CONT", "-x", "ferrum"]).status();
        let _ = self.action("exit", &[]);
        if let Some((mut h, stdin, _)) = self.helper.take() {
            drop(stdin);
            let _ = h.wait();
        }
        if let Some(mut c) = self.child.take() {
            // LinuxCNC shuts itself down when its display exits.
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(20) {
                if let Ok(Some(_)) = c.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if let Ok(None) = c.try_wait() {
                // Its whole process group, then the child itself by pid (a
                // group kill alone has been seen to leave it running here).
                let pg = c.id().to_string();
                let _ = Command::new("pkill").args(["-9", "-g", &pg]).status();
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        *self.ws_out.lock().unwrap() = None;
        cleanup(&self.rip);
    }
}
