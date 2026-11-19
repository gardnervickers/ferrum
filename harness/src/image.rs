//! Golden screenshots: a pixel counts as changed when any channel moved by
//! more than `CHANNEL_TOL`; the step fails when more than `max_pct` percent
//! of pixels changed. A diff image (changed pixels red over a dimmed copy)
//! goes next to the screenshot.

use std::path::Path;

const CHANNEL_TOL: i16 = 24;

pub struct Rgb {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

pub fn read(path: &Path) -> Result<Rgb, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(f));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let ch = info.color_type.samples();
    let mut px = Vec::with_capacity((info.width * info.height * 3) as usize);
    for p in buf[..info.buffer_size()].chunks(ch) {
        match ch {
            1 | 2 => px.extend([p[0], p[0], p[0]]),
            _ => px.extend(&p[..3]),
        }
    }
    Ok(Rgb {
        w: info.width,
        h: info.height,
        px,
    })
}

pub fn write(path: &Path, img: &Rgb) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.w, img.h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&img.px).map_err(|e| e.to_string())
}

/// Percent of pixels that changed, and the diff image.
pub fn compare(a: &Rgb, b: &Rgb) -> Result<(f64, Rgb), String> {
    if (a.w, a.h) != (b.w, b.h) {
        return Err(format!("size {}x{} vs golden {}x{}", a.w, a.h, b.w, b.h));
    }
    let mut changed = 0usize;
    let mut diff = Vec::with_capacity(a.px.len());
    for (p, q) in a.px.chunks(3).zip(b.px.chunks(3)) {
        let moved = p
            .iter()
            .zip(q)
            .any(|(x, y)| (*x as i16 - *y as i16).abs() > CHANNEL_TOL);
        if moved {
            changed += 1;
            diff.extend([255, 0, 0]);
        } else {
            diff.extend(p.iter().map(|x| x / 3));
        }
    }
    let pct = 100.0 * changed as f64 / (a.w as f64 * a.h as f64);
    Ok((
        pct,
        Rgb {
            w: a.w,
            h: a.h,
            px: diff,
        },
    ))
}
