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
    use crate::{Actions, CheckRow, CubeFace, DroPos, DroRow, FileOp, FileRow, GotoRow, HistoryRow, Live, MdiCard, MsgRow, NavItem, ConvField, JobOp, OffsetRow, PillData, ProbeItem, ProbeOpt, ProbeParam, ProbeSpot, SetupRow, SetupStep, StatusRow, ToolRow, WarmStage};
    include!(concat!(env!("OUT_DIR"), "/mirror.rs"));
}

/// Per-screen state, never mirrored.
const LOCAL: &[&str] = &["remote", "demo", "screen", "run-tab", "jog-tab", "files-tab", "drawer-open", "plot-dx", "plot-dy", "plot-landed", "pr-dialog"];
/// Browser-side edits the Pi accepts (two-way properties the actions read).
const FROM_REMOTE: &[&str] = &["mdi-text", "gcode-selected"];
/// Actions each screen handles for itself.
const LOCAL_ACTIONS: &[&str] = &["navigate", "plot-resize", "files-plot-resize", "exit"];

/// `FERRUM_HARNESS=1` (the test harness, ferrum/harness, simulator only):
/// the websocket also drives this screen's navigation, mirrors the
/// per-screen properties, allows continuous jogs and may quit.
fn harness() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FERRUM_HARNESS").is_ok_and(|v| v == "1"))
}

/// Binary frames are a tag byte, then a PNG: the backplot, or FILES' top
/// view of the selected program. In the browser demo the frame never leaves
/// the page, so it skips the PNG: the tag in lower case, width and height
/// (u32, little-endian), then the RGBA pixels.
const TAG_PLOT: u8 = b'P';
const TAG_FILES: u8 = b'F';

#[derive(Default)]
struct Pending {
    props: Map<String, Value>,
    png: Option<Arc<Vec<u8>>>,
    files_png: Option<Arc<Vec<u8>>>,
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
    /// FILES' picture last sent, and its frame.
    last_files: Option<slint::Image>,
    files_png: Option<Arc<Vec<u8>>>,
    ticks: u32,
}

pub fn start(ui: &MainWindow, port: u16, root: PathBuf) -> Result<(slint::Timer, std::rc::Rc<std::cell::RefCell<Remote>>), String> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    // The browser demo: one screen, in the same page (demo.rs).
    #[cfg(target_os = "wasi")]
    {
        let _ = (port, root);
        let client = Arc::new(Client { pending: Mutex::new(Pending::default()), fresh: Mutex::new(true) });
        shared.lock().unwrap().clients.push(client.clone());
        DEMO_CLIENT.with(|c| *c.borrow_mut() = Some(client));
    }
    #[cfg(not(target_os = "wasi"))]
    {
        let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("remote: port {port}: {e}"))?;
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
    let remote = std::rc::Rc::new(std::cell::RefCell::new(Remote { shared, last: Map::new(), last_plot: None, png: None, last_files: None, files_png: None, ticks: 0 }));
    let timer = slint::Timer::default();
    let (r, weak) = (remote.clone(), ui.as_weak());
    // The browser demo's screen is in the same page: mirror at its poll rate.
    let every = Duration::from_millis(if cfg!(target_os = "wasi") { 20 } else { 50 });
    timer.start(slint::TimerMode::Repeated, every, move || {
        if let Some(ui) = weak.upgrade() {
            r.borrow_mut().tick(&ui);
            #[cfg(target_os = "wasi")]
            demo_flush();
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
            self.last_files = None;
            self.files_png = None;
            return;
        }
        let live = ui.global::<Live>();
        let mut diff = Map::new();
        for &name in mirror::PROPS {
            if LOCAL.contains(&name) && !harness() {
                continue;
            }
            let Some(v) = mirror::get_prop(&live, name) else { continue };
            if self.last.get(name) != Some(&v) {
                self.last.insert(name.to_string(), v.clone());
                diff.insert(name.to_string(), v);
            }
        }
        // The harness checks the screen watchdog's view of drawing
        // (failsafe.rs): the longest gap between drawn frames, dropped taps.
        if harness() {
            let (_, gap, dropped) = crate::failsafe::stats();
            for (name, v) in [("failsafe-max-gap-ms", gap), ("failsafe-dropped", dropped)] {
                let v = Value::from(v);
                if self.last.get(name) != Some(&v) {
                    self.last.insert(name.to_string(), v.clone());
                    diff.insert(name.to_string(), v);
                }
            }
        }
        // The backplot changes at most ten times a second; send it at 4 Hz
        // (each change in the browser demo, which has no network between).
        self.ticks = self.ticks.wrapping_add(1);
        let every = if cfg!(target_os = "wasi") { 1 } else { 5 };
        let mut png = None;
        if self.ticks % every == 0 || self.png.is_none() {
            let v = crate::plot::version();
            if self.last_plot != Some(v) {
                // A GPU texture has no CPU copy: its pixels come from a later frame.
                if let Some(buf) = live.get_plot().to_rgba8().or_else(|| crate::plot::gpu_pixels(ui)) {
                    self.png = encode(TAG_PLOT, &buf).map(Arc::new);
                    self.last_plot = Some(v);
                    png = self.png.clone();
                }
            }
        }
        // FILES' picture changes only when a program or op is picked.
        let mut files_png = None;
        let files = live.get_files_plot();
        if self.last_files.as_ref() != Some(&files) {
            self.files_png = files.to_rgba8().and_then(|b| encode(TAG_FILES, &b)).map(Arc::new);
            self.last_files = Some(files);
            files_png = self.files_png.clone();
        }
        for c in clients {
            let mut fresh = c.fresh.lock().unwrap();
            let mut p = c.pending.lock().unwrap();
            if *fresh {
                *fresh = false;
                p.props = self.last.clone();
                p.png = self.png.clone();
                p.files_png = self.files_png.clone();
            } else {
                p.props.extend(diff.clone());
                if png.is_some() {
                    p.png = png.clone();
                }
                if files_png.is_some() {
                    p.files_png = files_png.clone();
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

fn encode(tag: u8, buf: &slint::SharedPixelBuffer<slint::Rgba8Pixel>) -> Option<Vec<u8>> {
    if buf.width() == 0 {
        return None;
    }
    if cfg!(target_os = "wasi") {
        let mut out = Vec::with_capacity(9 + buf.as_bytes().len());
        out.push(tag.to_ascii_lowercase());
        out.extend_from_slice(&buf.width().to_le_bytes());
        out.extend_from_slice(&buf.height().to_le_bytes());
        out.extend_from_slice(buf.as_bytes());
        return Some(out);
    }
    let mut out = vec![tag];
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

/// A browser is connected (it mirrors screens this one may not show).
pub fn watching() -> bool {
    REMOTE.with(|x| x.borrow().as_ref().is_some_and(|r| !r.borrow().shared.lock().unwrap().clients.is_empty()))
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
        let (props, png, files_png, closed) = {
            let mut p = client.pending.lock().unwrap();
            (std::mem::take(&mut p.props), p.png.take(), p.files_png.take(), p.closed)
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
        if png.into_iter().chain(files_png).any(|png| ws.send(Message::binary(png.as_ref().clone())).is_err()) {
            break;
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
                        // The harness also opens the PROBE screen's dialogs.
                        if FROM_REMOTE.contains(&k.as_str()) || (harness() && k == "pr-dialog") {
                            r.borrow_mut().accept(&ui, &client, k, v.clone());
                        }
                    }
                }
            });
        }
    });
}

#[cfg(target_os = "wasi")]
thread_local! {
    static DEMO_CLIENT: std::cell::RefCell<Option<Arc<Client>>> = const { std::cell::RefCell::new(None) };
}

// The demo page's side of the link (web/static/demo.js).
#[cfg(target_os = "wasi")]
#[link(wasm_import_module = "ferrum")]
extern "C" {
    fn send_text(ptr: *const u8, len: usize);
    fn send_png(ptr: *const u8, len: usize);
}

/// Hand the demo page what is pending, as the websocket loop would.
#[cfg(target_os = "wasi")]
fn demo_flush() {
    let Some(client) = DEMO_CLIENT.with(|c| c.borrow().clone()) else { return };
    let (props, png, files_png) = {
        let mut p = client.pending.lock().unwrap();
        (std::mem::take(&mut p.props), p.png.take(), p.files_png.take())
    };
    if !props.is_empty() {
        let text = serde_json::json!({ "p": props }).to_string();
        unsafe { send_text(text.as_ptr(), text.len()) };
    }
    for png in png.into_iter().chain(files_png) {
        unsafe { send_png(png.as_ptr(), png.len()) };
    }
}

/// A message from the demo page, as if it came over the websocket.
#[cfg(target_os = "wasi")]
pub fn demo_receive(text: &str) {
    let Some(client) = DEMO_CLIENT.with(|c| c.borrow().clone()) else { return };
    let Some(weak) = crate::demo::ui_weak() else { return };
    handle(&weak, &client, text);
}

/// Run a browser action here, after the checks only the Pi can make.
fn admit(ui: &MainWindow, name: &str, args: &[Value]) {
    let live = ui.global::<Live>();
    // The demo's simulated mill has one screen to serve: it goes where the
    // page goes, so FILES draws its picture (demo.rs).
    if LOCAL_ACTIONS.contains(&name) && !harness() && !(cfg!(target_os = "wasi") && name == "navigate") {
        return;
    }
    if name == "failsafe-reset-gap" && harness() {
        crate::failsafe::reset_max_gap();
        return;
    }
    // The browser demo's machine is simulated in the same page: no lost
    // "stop" can leave anything moving, so it jogs like the touchscreen.
    if name == "jog-start" && live.get_jog_step() == "CONT" && !harness() && !cfg!(target_os = "wasi") {
        live.set_message("Remote jogs are step-only: pick a step size".into());
        live.set_message_error(true);
        return;
    }
    mirror::invoke_action(&ui.global::<Actions>(), name, args);
}
