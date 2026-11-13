//! Browser build of the machine screen.
//!
//! The same Slint UI as the touchscreen, drawn by the software renderer into
//! a 1920x1080 canvas that CSS scales to the window. A websocket to millgui on
//! the Pi carries `Live` property changes in and `Actions` calls out; the Pi
//! decides what to run. Navigation (screen, tabs, message drawer) is local.

#![cfg(target_arch = "wasm32")]

use serde_json::{json, Map, Value};
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, SharedString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData, KeyboardEvent, MessageEvent, PointerEvent, WebSocket, WheelEvent};

slint::include_modules!();

#[allow(unused_parens, clippy::all)]
mod mirror {
    use super::{Actions, CheckRow, DroRow, FileRow, GotoRow, Live, MdiCard, MsgRow, NavItem, ConvField, JobOp, OffsetRow, PillData, ProbeItem, ProbeParam, ProbeSpot, StatusRow, ToolRow, WarmStage};
    include!(concat!(env!("OUT_DIR"), "/mirror.rs"));
}

/// The touchscreen's size: the layout's minimum. A browser window of another
/// shape gets extra room in that direction rather than black bars.
const W: u32 = 1920;
const H: u32 = 1080;
/// Two-way properties: edits here go to the Pi.
const TO_PI: &[&str] = &["mdi-text", "gcode-selected"];
/// Actions this screen handles itself.
const LOCAL_ACTIONS: &[&str] = &["navigate", "plot-resize", "exit"];

struct WebPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

fn now_ms() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map(|p| p.now()).unwrap_or(0.0)
}

impl Platform for WebPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        Duration::from_secs_f64(now_ms() / 1000.0)
    }
}

struct App {
    ui: MainWindow,
    window: Rc<MinimalSoftwareWindow>,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    buf: RefCell<Vec<PremultipliedRgbaColor>>,
    ws: RefCell<Option<WebSocket>>,
    known: RefCell<Map<String, Value>>,
    size: Cell<(u32, u32)>,
    /// After a resize the reused buffer holds nothing valid: repaint all of it.
    full: Cell<bool>,
    perf: RefCell<Perf>,
}

/// Frame timings, logged to the console every two seconds with `?perf`.
#[derive(Default)]
struct Perf {
    on: bool,
    frames: u32,
    render_ms: f64,
    put_ms: f64,
    px: f64,
    since: f64,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn app() -> Option<Rc<App>> {
    APP.with(|a| a.borrow().clone())
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    window.set_size(PhysicalSize::new(W, H));
    slint::platform::set_platform(Box::new(WebPlatform { window: window.clone() })).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let ui = MainWindow::new().map_err(|e| JsValue::from_str(&e.to_string()))?;
    ui.show().map_err(|e| JsValue::from_str(&e.to_string()))?;
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));

    let live = ui.global::<Live>();
    live.set_remote(true);
    live.set_connected(false);
    live.set_message("Connecting to the mill…".into());

    let actions = ui.global::<Actions>();
    mirror::forward_actions(&actions, Rc::new(|name: &str, args: Vec<Value>| send(json!({ "a": name, "args": args }))), LOCAL_ACTIONS);
    let weak = ui.as_weak();
    actions.on_navigate(move |id| {
        if let Some(ui) = weak.upgrade() {
            // As on the touchscreen: an open keypad or picker belongs to the
            // screen being left (they are shared, so the mill closes them).
            let live = ui.global::<Live>();
            if live.get_keypad_open() {
                send(json!({ "a": "keypad-done", "args": [false] }));
            }
            if live.get_pick_open() {
                send(json!({ "a": "pick", "args": [-1] }));
            }
            if id == "files" {
                send(json!({ "a": "files-refresh", "args": [] }));
            }
            live.set_screen(id);
        }
    });

    let doc = web_sys::window().unwrap().document().unwrap();
    let canvas: HtmlCanvasElement = doc.get_element_by_id("screen").unwrap().dyn_into()?;
    let ctx: CanvasRenderingContext2d = canvas.get_context("2d")?.unwrap().dyn_into()?;
    let app = Rc::new(App {
        ui,
        window,
        canvas,
        ctx,
        buf: RefCell::new(vec![PremultipliedRgbaColor::default(); (W * H) as usize]),
        ws: RefCell::new(None),
        known: RefCell::new(Map::new()),
        size: Cell::new((0, 0)),
        full: Cell::new(true),
        perf: RefCell::new(Perf { on: web_sys::window().unwrap().location().search().unwrap_or_default().contains("perf"), ..Default::default() }),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    resize(&app);
    let on_resize = Closure::<dyn FnMut()>::new(|| {
        if let Some(app) = self::app() {
            resize(&app);
        }
    });
    web_sys::window().unwrap().add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref())?;
    on_resize.forget();
    input(&app)?;
    connect();
    frame();
    Ok(())
}

/// Fit the window: at least 1920x1080 logical pixels, stretched in the
/// direction the browser window has extra room, then scaled down by CSS.
fn resize(app: &App) {
    let win = web_sys::window().unwrap();
    let vw = win.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(W as f64).max(1.0);
    let vh = win.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(H as f64).max(1.0);
    let k = (W as f64 / vw).max(H as f64 / vh);
    let size = ((vw * k).round() as u32, (vh * k).round() as u32);
    if size == app.size.get() {
        return;
    }
    app.size.set(size);
    app.canvas.set_width(size.0);
    app.canvas.set_height(size.1);
    *app.buf.borrow_mut() = vec![PremultipliedRgbaColor::default(); (size.0 * size.1) as usize];
    app.window.set_size(PhysicalSize::new(size.0, size.1));
    app.full.set(true);
    app.window.request_redraw();
}

fn send(msg: Value) {
    if let Some(app) = app() {
        if let Some(ws) = app.ws.borrow().as_ref() {
            if ws.ready_state() == WebSocket::OPEN {
                let _ = ws.send_with_str(&msg.to_string());
            }
        }
    }
}

fn connect() {
    let win = web_sys::window().unwrap();
    let loc = win.location();
    let scheme = if loc.protocol().unwrap_or_default() == "https:" { "wss" } else { "ws" };
    let url = format!("{scheme}://{}/ws", loc.host().unwrap_or_default());
    let Ok(ws) = WebSocket::new(&url) else { return retry() };
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
    let on_msg = Closure::<dyn FnMut(MessageEvent)>::new(|e: MessageEvent| {
        let Some(app) = app() else { return };
        if let Some(text) = e.data().as_string() {
            receive(&app, &text);
        } else if let Ok(buf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
            if let Some(img) = decode_png(&js_sys::Uint8Array::new(&buf).to_vec()) {
                app.ui.global::<Live>().set_plot(img);
            }
        }
    });
    ws.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
    on_msg.forget();
    let on_close = Closure::<dyn FnMut(web_sys::CloseEvent)>::new(|_| {
        if let Some(app) = app() {
            let live = app.ui.global::<Live>();
            live.set_connected(false);
            live.set_message("Lost the connection to the mill; retrying…".into());
            live.set_message_error(true);
            app.known.borrow_mut().clear();
            *app.ws.borrow_mut() = None;
        }
        retry();
    });
    ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    on_close.forget();
    if let Some(app) = app() {
        *app.ws.borrow_mut() = Some(ws);
    }
}

fn retry() {
    let cb = Closure::once_into_js(connect);
    let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), 2000);
}

fn receive(app: &App, text: &str) {
    let Ok(msg) = serde_json::from_str::<Value>(text) else { return };
    let Some(props) = msg["p"].as_object() else { return };
    let live = app.ui.global::<Live>();
    let mut known = app.known.borrow_mut();
    for (k, v) in props {
        if TO_PI.contains(&k.as_str()) {
            known.insert(k.clone(), v.clone());
        }
        mirror::set_prop(&live, k, v);
    }
}

fn decode_png(bytes: &[u8]) -> Option<slint::Image> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&buf[..info.buffer_size()], info.width, info.height);
    Some(slint::Image::from_rgba8(pixels))
}

/// One animation frame: timers, outgoing edits, and a repaint if needed.
fn frame() {
    if let Some(app) = app() {
        slint::platform::update_timers_and_animations();
        let live = app.ui.global::<Live>();
        let mut out = Map::new();
        {
            let mut known = app.known.borrow_mut();
            for &name in TO_PI {
                if let Some(v) = mirror::get_prop(&live, name) {
                    if known.get(name).is_some_and(|k| *k != v) {
                        known.insert(name.into(), v.clone());
                        out.insert(name.into(), v);
                    }
                }
            }
        }
        if !out.is_empty() {
            send(json!({ "s": out }));
        }
        let (w, h) = app.size.get();
        app.window.draw_if_needed(|r| {
            let t0 = now_ms();
            let mut buf = app.buf.borrow_mut();
            let full = app.full.replace(false);
            if full {
                r.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            }
            let region = r.render(&mut buf[..], w as usize);
            if full {
                r.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            }
            let t1 = now_ms();
            // Copy out only the repainted rectangle.
            let (o, s) = (region.bounding_box_origin(), region.bounding_box_size());
            let (x0, y0) = (o.x.max(0) as u32, o.y.max(0) as u32);
            let (bw, bh) = (s.width.min(w - x0.min(w)), s.height.min(h - y0.min(h)));
            if bw > 0 && bh > 0 {
                let mut rect = Vec::with_capacity((bw * bh * 4) as usize);
                for y in y0..y0 + bh {
                    let row = &buf[(y * w + x0) as usize..(y * w + x0 + bw) as usize];
                    // PremultipliedRgbaColor is four u8s in RGBA order; the frame is opaque.
                    rect.extend_from_slice(unsafe { std::slice::from_raw_parts(row.as_ptr() as *const u8, row.len() * 4) });
                }
                if let Ok(img) = ImageData::new_with_u8_clamped_array_and_sh(Clamped(&rect), bw, bh) {
                    let _ = app.ctx.put_image_data(&img, x0 as f64, y0 as f64);
                }
            }
            let mut p = app.perf.borrow_mut();
            if p.on {
                let t2 = now_ms();
                p.frames += 1;
                p.render_ms += t1 - t0;
                p.put_ms += t2 - t1;
                p.px += (bw * bh) as f64;
                if t2 - p.since > 2000.0 {
                    let n = p.frames.max(1) as f64;
                    web_sys::console::log_1(&format!("perf: {} frames, render {:.1} ms, copy {:.1} ms, {:.0} kpx per frame", p.frames, p.render_ms / n, p.put_ms / n, p.px / n / 1000.0).into());
                    *p = Perf { on: true, since: t2, ..Default::default() };
                }
            }
        });
    }
    let cb = Closure::once_into_js(frame);
    let _ = web_sys::window().unwrap().request_animation_frame(cb.unchecked_ref());
}

fn pos(app: &App, x: f64, y: f64) -> LogicalPosition {
    let r = app.canvas.get_bounding_client_rect();
    let (w, h) = app.size.get();
    let sx = w as f64 / r.width().max(1.0);
    let sy = h as f64 / r.height().max(1.0);
    LogicalPosition::new(((x - r.left()) * sx) as f32, ((y - r.top()) * sy) as f32)
}

fn key_text(k: &str) -> Option<SharedString> {
    let special = match k {
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Enter" => Key::Return,
        "Tab" => Key::Tab,
        "Escape" => Key::Escape,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "Home" => Key::Home,
        "End" => Key::End,
        "Shift" => Key::Shift,
        "Control" => Key::Control,
        "Alt" => Key::Alt,
        "Meta" => Key::Meta,
        _ if k.chars().count() == 1 => return Some(k.into()),
        _ => return None,
    };
    Some(special.into())
}

fn input(this: &Rc<App>) -> Result<(), JsValue> {
    let target = this.canvas.clone();
    let add = |name: &str, f: Box<dyn FnMut(PointerEvent)>| -> Result<(), JsValue> {
        let c = Closure::<dyn FnMut(PointerEvent)>::new(f);
        target.add_event_listener_with_callback(name, c.as_ref().unchecked_ref())?;
        c.forget();
        Ok(())
    };
    add(
        "pointerdown",
        Box::new(|e: PointerEvent| {
            let Some(app) = app() else { return };
            let _ = app.canvas.set_pointer_capture(e.pointer_id());
            e.prevent_default();
            let position = pos(&app, e.client_x() as f64, e.client_y() as f64);
            app.window.dispatch_event(WindowEvent::PointerMoved { position });
            app.window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        }),
    )?;
    add(
        "pointermove",
        Box::new(|e: PointerEvent| {
            let Some(app) = app() else { return };
            let position = pos(&app, e.client_x() as f64, e.client_y() as f64);
            app.window.dispatch_event(WindowEvent::PointerMoved { position });
        }),
    )?;
    for name in ["pointerup", "pointercancel"] {
        add(
            name,
            Box::new(|e: PointerEvent| {
                let Some(app) = app() else { return };
                let position = pos(&app, e.client_x() as f64, e.client_y() as f64);
                app.window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
            }),
        )?;
    }
    add(
        "pointerleave",
        Box::new(|_| {
            if let Some(app) = app() {
                app.window.dispatch_event(WindowEvent::PointerExited);
            }
        }),
    )?;
    let wheel = Closure::<dyn FnMut(WheelEvent)>::new(|e: WheelEvent| {
        let Some(app) = app() else { return };
        e.prevent_default();
        let position = pos(&app, e.client_x() as f64, e.client_y() as f64);
        app.window.dispatch_event(WindowEvent::PointerScrolled { position, delta_x: -e.delta_x() as f32, delta_y: -e.delta_y() as f32 });
    });
    target.add_event_listener_with_callback("wheel", wheel.as_ref().unchecked_ref())?;
    wheel.forget();
    let doc = web_sys::window().unwrap();
    for (name, down) in [("keydown", true), ("keyup", false)] {
        let c = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            let Some(app) = app() else { return };
            if e.meta_key() || e.ctrl_key() {
                return; // leave browser shortcuts alone
            }
            let Some(text) = key_text(&e.key()) else { return };
            e.prevent_default();
            app.window.dispatch_event(if down { WindowEvent::KeyPressed { text } } else { WindowEvent::KeyReleased { text } });
        });
        doc.add_event_listener_with_callback(name, c.as_ref().unchecked_ref())?;
        c.forget();
    }
    Ok(())
}
