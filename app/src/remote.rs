//! Remote (web) access: serves the browser build and mirrors this screen to
//! it over a websocket (ARCHITECTURE.md, Remote access).
//!
//! The browser runs the same Slint UI. The Pi sends `Live` property changes
//! (and the backplot as PNG); the browser sends `Actions` calls, which run
//! here exactly as touchscreen taps do. Navigation stays local to each screen.
//!
//! Correctness: the Pi admits every remote command (the browser greying out a
//! button is only UX), and remote jogs are step-only, so a lost "stop" cannot
//! leave an axis moving. Availability: each client has a pending-state slot
//! that merges updates (newest wins), so a slow client never stalls the UI
//! thread or the worker.

use crate::{Actions, Live, MainWindow};
use serde_json::{Map, Value};
use slint::ComponentHandle;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tungstenite::Message;

#[allow(unused_parens, clippy::all)]
mod mirror {
    use crate::{Actions, CheckRow, CubeFace, DroRow, FileRow, GotoRow, HistoryRow, Live, MdiCard, MsgRow, NavItem, ConvField, JobOp, OffsetRow, PillData, ProbeItem, ProbeMove, ProbeOpt, ProbeParam, ProbeSpot, StatusRow, ToolRow, WarmStage};
    include!(concat!(env!("OUT_DIR"), "/mirror.rs"));
}

/// Per-screen state, never mirrored.
const LOCAL: &[&str] = &["remote", "screen", "run-tab", "jog-tab", "drawer-open", "plot-dx", "plot-dy", "plot-landed"];
/// Browser-side edits the Pi accepts (two-way properties the actions read).
const FROM_REMOTE: &[&str] = &["mdi-text", "gcode-selected"];
/// Actions each screen handles for itself.
const LOCAL_ACTIONS: &[&str] = &["navigate", "plot-resize", "exit"];

#[derive(Default)]
struct Pending {
    props: Map<String, Value>,
    png: Option<Arc<Vec<u8>>>,
    closed: bool,
}

struct Client {
    pending: Mutex<Pending>,
    fresh: Mutex<bool>,
}

#[derive(Default)]
struct Shared {
    clients: Vec<Arc<Client>>,
}

pub struct Remote {
    shared: Arc<Mutex<Shared>>,
    last: Map<String, Value>,
    /// The backplot version last encoded (see `plot::version`).
    last_plot: Option<u64>,
    png: Option<Arc<Vec<u8>>>,
    ticks: u32,
}

pub fn start(ui: &MainWindow, port: u16, root: PathBuf) -> Result<(slint::Timer, std::rc::Rc<std::cell::RefCell<Remote>>), String> {
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("remote: port {port}: {e}"))?;
    let shared = Arc::new(Mutex::new(Shared::default()));
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        std::thread::Builder::new()
            .name("remote".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (shared, weak, root) = (shared.clone(), weak.clone(), root.clone());
                    let _ = std::thread::Builder::new().name("remote-client".into()).spawn(move || serve(stream, shared, weak, root));
                }
            })
            .map_err(|e| e.to_string())?;
    }
    let remote = std::rc::Rc::new(std::cell::RefCell::new(Remote { shared, last: Map::new(), last_plot: None, png: None, ticks: 0 }));
    let timer = slint::Timer::default();
    let (r, weak) = (remote.clone(), ui.as_weak());
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        if let Some(ui) = weak.upgrade() {
            r.borrow_mut().tick(&ui);
        }
    });
    Ok((timer, remote))
}

impl Remote {
    /// Diff the mirrored properties and queue the changes for every client.
    fn tick(&mut self, ui: &MainWindow) {
        let clients: Vec<Arc<Client>> = self.shared.lock().unwrap().clients.clone();
        if clients.is_empty() {
            self.last.clear();
            self.last_plot = None;
            self.png = None;
            return;
        }
        let live = ui.global::<Live>();
        let mut diff = Map::new();
        for &name in mirror::PROPS {
            if LOCAL.contains(&name) {
                continue;
            }
            let Some(v) = mirror::get_prop(&live, name) else { continue };
            if self.last.get(name) != Some(&v) {
                self.last.insert(name.to_string(), v.clone());
                diff.insert(name.to_string(), v);
            }
        }
        // The backplot changes at most ten times a second; send it at 4 Hz.
        self.ticks = self.ticks.wrapping_add(1);
        let mut png = None;
        if self.ticks % 5 == 0 || self.png.is_none() {
            let v = crate::plot::version();
            if self.last_plot != Some(v) {
                // A GPU texture has no CPU copy: its pixels come from a later frame.
                if let Some(buf) = live.get_plot().to_rgba8().or_else(|| crate::plot::gpu_pixels(ui)) {
                    self.png = encode(&buf).map(Arc::new);
                    self.last_plot = Some(v);
                    png = self.png.clone();
                }
            }
        }
        for c in clients {
            let mut fresh = c.fresh.lock().unwrap();
            let mut p = c.pending.lock().unwrap();
            if *fresh {
                *fresh = false;
                p.props = self.last.clone();
                p.png = self.png.clone();
            } else {
                p.props.extend(diff.clone());
                if png.is_some() {
                    p.png = png.clone();
                }
            }
        }
    }

    /// A browser changed a two-way property: apply it and pass it on.
    fn accept(&mut self, ui: &MainWindow, from: &Arc<Client>, name: &str, v: Value) {
        mirror::set_prop(&ui.global::<Live>(), name, &v);
        self.last.insert(name.to_string(), v.clone());
        for c in self.shared.lock().unwrap().clients.iter() {
            if !Arc::ptr_eq(c, from) {
                c.pending.lock().unwrap().props.insert(name.to_string(), v.clone());
            }
        }
    }
}

fn encode(buf: &slint::SharedPixelBuffer<slint::Rgba8Pixel>) -> Option<Vec<u8>> {
    if buf.width() == 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, buf.width(), buf.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header().ok()?;
    w.write_image_data(buf.as_bytes()).ok()?;
    drop(w);
    Some(out)
}

thread_local! {
    static REMOTE: std::cell::RefCell<Option<std::rc::Rc<std::cell::RefCell<Remote>>>> = const { std::cell::RefCell::new(None) };
}

pub fn install(r: std::rc::Rc<std::cell::RefCell<Remote>>) {
    REMOTE.with(|x| *x.borrow_mut() = Some(r));
}

fn serve(stream: TcpStream, shared: Arc<Mutex<Shared>>, weak: slint::Weak<MainWindow>, root: PathBuf) {
    let mut head = [0u8; 2048];
    let n = stream.peek(&mut head).unwrap_or(0);
    let req = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
    if req.contains("upgrade: websocket") {
        websocket(stream, shared, weak);
    } else {
        let _ = static_file(stream, &root);
    }
}

fn static_file(mut stream: TcpStream, root: &std::path::Path) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/");
    let name = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    // Only plain names inside the bundle directory.
    let ok = !name.contains("..") && !name.contains('/') && !name.is_empty();
    // Prefer the gzipped copy scripts/build-web makes (the wasm is ~7 MB raw).
    let gzip = req.to_ascii_lowercase().contains("accept-encoding:") && req.to_ascii_lowercase().contains("gzip");
    let gz = if ok && gzip { std::fs::read(root.join(format!("{name}.gz"))).ok() } else { None };
    let enc = if gz.is_some() { "Content-Encoding: gzip\r\n" } else { "" };
    let body = gz.or_else(|| if ok { std::fs::read(root.join(name)).ok() } else { None });
    match body {
        Some(b) => {
            let ty = match name.rsplit('.').next() {
                Some("html") => "text/html; charset=utf-8",
                Some("js") => "text/javascript",
                Some("wasm") => "application/wasm",
                Some("png") => "image/png",
                _ => "application/octet-stream",
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\n{enc}Content-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n", b.len())?;
            stream.write_all(&b)
        }
        None => stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found"),
    }
}

fn websocket(stream: TcpStream, shared: Arc<Mutex<Shared>>, weak: slint::Weak<MainWindow>) {
    let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let Ok(mut ws) = tungstenite::accept(stream) else { return };
    let _ = ws.get_mut().set_read_timeout(Some(Duration::from_millis(40)));
    let _ = ws.get_mut().set_nodelay(true);
    let client = Arc::new(Client { pending: Mutex::new(Pending::default()), fresh: Mutex::new(true) });
    shared.lock().unwrap().clients.push(client.clone());
    notify(&weak, format!("Remote screen connected ({peer})"));
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => handle(&weak, &client, t.as_str()),
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        let (props, png, closed) = {
            let mut p = client.pending.lock().unwrap();
            (std::mem::take(&mut p.props), p.png.take(), p.closed)
        };
        if closed {
            break;
        }
        if !props.is_empty() {
            let text = serde_json::json!({ "p": props }).to_string();
            if ws.send(Message::text(text)).is_err() {
                break;
            }
        }
        if let Some(png) = png {
            if ws.send(Message::binary(png.as_ref().clone())).is_err() {
                break;
            }
        }
    }
    shared.lock().unwrap().clients.retain(|c| !Arc::ptr_eq(c, &client));
    notify(&weak, format!("Remote screen disconnected ({peer})"));
}

fn notify(weak: &slint::Weak<MainWindow>, text: String) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            let live = ui.global::<Live>();
            live.set_message(text.into());
            live.set_message_error(false);
        }
    });
}

fn handle(weak: &slint::Weak<MainWindow>, client: &Arc<Client>, text: &str) {
    let Ok(msg) = serde_json::from_str::<Value>(text) else { return };
    let weak = weak.clone();
    let client = client.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = weak.upgrade() else { return };
        if let Some(name) = msg["a"].as_str() {
            let args = msg["args"].as_array().cloned().unwrap_or_default();
            admit(&ui, name, &args);
        }
        if let Some(set) = msg["s"].as_object() {
            REMOTE.with(|r| {
                if let Some(r) = r.borrow().as_ref() {
                    for (k, v) in set {
                        if FROM_REMOTE.contains(&k.as_str()) {
                            r.borrow_mut().accept(&ui, &client, k, v.clone());
                        }
                    }
                }
            });
        }
    });
}

/// Run a browser action here, after the checks only the Pi can make.
fn admit(ui: &MainWindow, name: &str, args: &[Value]) {
    let live = ui.global::<Live>();
    if LOCAL_ACTIONS.contains(&name) {
        return;
    }
    if name == "jog-start" && live.get_jog_step() == "CONT" {
        live.set_message("Remote jogs are step-only: pick a step size".into());
        live.set_message_error(true);
        return;
    }
    mirror::invoke_action(&ui.global::<Actions>(), name, args);
}
