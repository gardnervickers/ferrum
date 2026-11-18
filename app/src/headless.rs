//! `--screenshot <png>`: render one frame against the simulated machine
//! without a display, for reviewing screens on the Mac or in CI.

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{PhysicalSize, Rgb8Pixel};
use std::path::Path;
use std::rc::Rc;

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

pub fn install(width: u32, height: u32) -> Rc<MinimalSoftwareWindow> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(PhysicalSize::new(width, height));
    slint::platform::set_platform(Box::new(Headless { window: window.clone() })).expect("set headless platform");
    window
}

/// Draw for `ms`, so entrances (which start when first drawn) play out.
pub fn settle(window: &MinimalSoftwareWindow, ms: u64) {
    let size = window.size();
    let (w, h) = (size.width as usize, size.height as usize);
    let mut buf = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; w * h];
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_millis(ms) {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut buf, w);
        });
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}

pub fn save(window: &MinimalSoftwareWindow, path: &Path) -> Result<(), String> {
    let size = window.size();
    let (w, h) = (size.width as usize, size.height as usize);
    let mut buf = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; w * h];
    // Twice, with the backplot's size timer (250 ms) firing in between.
    for pass in 0..2 {
        if pass == 1 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut buf, w);
        });
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    let bytes: Vec<u8> = buf.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    wr.write_image_data(&bytes).map_err(|e| e.to_string())
}
