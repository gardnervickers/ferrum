//! Antialiased lines straight into a coverage mask, for the software
//! backplot. tiny-skia turns each stroke into a filled outline and sorts its
//! edges, which costs ~2 µs per move; this touches only the pixels a line
//! covers. A layer (all rapids, or one op's feeds) is drawn as coverage with
//! `max`, then blended once in its color, so crossings and joins don't
//! darken: the same look as stroking the layer as one path.

use tiny_skia::Pixmap;

pub struct Coverage {
    w: usize,
    h: usize,
    buf: Vec<u8>,
    /// Touched pixels: x0, y0, x1, y1 (exclusive), empty when x0 >= x1.
    bbox: (usize, usize, usize, usize),
}

impl Coverage {
    pub fn new(w: u32, h: u32) -> Self {
        let (w, h) = (w as usize, h as usize);
        Self { w, h, buf: vec![0; w * h], bbox: (w, h, 0, 0) }
    }

    /// A line from `a` to `b` (pixels) `width` wide with round ends.
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32) {
        let r = width * 0.5;
        // Pixels within r + 0.5 of the line get some coverage.
        let reach = r + 0.5;
        let (wf, hf) = (self.w as f32, self.h as f32);
        if a.0.max(b.0) + reach < 0.0 || a.0.min(b.0) - reach > wf || a.1.max(b.1) + reach < 0.0 || a.1.min(b.1) - reach > hf {
            return;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        // Walk the longer axis; across it, cover the line's thickness.
        let steep = dy.abs() > dx.abs();
        let (a, b, dx, dy) = if steep { ((a.1, a.0), (b.1, b.0), dy, dx) } else { (a, b, dx, dy) };
        let (lim_u, lim_v) = if steep { (self.h, self.w) } else { (self.w, self.h) };
        let u0 = ((a.0.min(b.0) - reach).floor().max(0.0)) as usize;
        let u1 = ((a.0.max(b.0) + reach).ceil().min(lim_u as f32)) as usize;
        let slope = if dx.abs() > 1e-6 { dy / dx } else { 0.0 };
        // Rows either side of the center at each column (thicker when steep-ish).
        let span = reach * (1.0 + slope * slope).sqrt() + 0.5;
        let reach2 = reach * reach;
        // Fully covered inside r - 0.5.
        let solid2 = if r > 0.5 { (r - 0.5) * (r - 0.5) } else { -1.0 };
        let inv_len2 = if len2 > 1e-12 { 1.0 / len2 } else { 0.0 };
        let (mut bx0, mut by0, mut bx1, mut by1) = self.bbox;
        for u in u0..u1 {
            let pu = u as f32 + 0.5;
            // Center of the line at this column, held to the segment's ends.
            let t = if dx.abs() > 1e-6 { ((pu - a.0) / dx).clamp(0.0, 1.0) } else { 0.5 };
            let vc = a.1 + dy * t;
            let v0 = ((vc - span).floor().max(0.0)) as usize;
            let v1 = ((vc + span).ceil().min(lim_v as f32)) as usize;
            for v in v0..v1 {
                let pv = v as f32 + 0.5;
                // Distance from the pixel center to the segment.
                let (qx, qy) = (pu - a.0, pv - a.1);
                let s = ((qx * dx + qy * dy) * inv_len2).clamp(0.0, 1.0);
                let (ex, ey) = (qx - dx * s, qy - dy * s);
                let d2 = ex * ex + ey * ey;
                if d2 >= reach2 {
                    continue;
                }
                let c = if d2 <= solid2 { 255 } else { ((reach - d2.sqrt()).min(1.0) * 255.0) as u8 };
                let (x, y) = if steep { (v, u) } else { (u, v) };
                let px = &mut self.buf[y * self.w + x];
                if c > *px {
                    *px = c;
                }
                bx0 = bx0.min(x);
                by0 = by0.min(y);
                bx1 = bx1.max(x + 1);
                by1 = by1.max(y + 1);
            }
        }
        self.bbox = (bx0, by0, bx1, by1);
    }

    /// Blend the coverage onto `pm` in `rgb` at `alpha`, then clear it.
    pub fn paint(&mut self, pm: &mut Pixmap, rgb: [u8; 3], alpha: u8) {
        let (x0, y0, x1, y1) = self.bbox;
        if x0 >= x1 {
            return;
        }
        let pw = pm.width() as usize;
        let data = pm.data_mut();
        for y in y0..y1 {
            let row = &mut self.buf[y * self.w + x0..y * self.w + x1];
            for (i, c) in row.iter_mut().enumerate() {
                if *c == 0 {
                    continue;
                }
                // Source alpha, premultiplied source over the premultiplied pixmap.
                let sa = (*c as u32 * alpha as u32 + 127) / 255;
                *c = 0;
                let o = (y * pw + x0 + i) * 4;
                let px = &mut data[o..o + 4];
                let inv = 255 - sa;
                for k in 0..3 {
                    px[k] = ((rgb[k] as u32 * sa + px[k] as u32 * inv + 127) / 255) as u8;
                }
                px[3] = (sa + (px[3] as u32 * inv + 127) / 255) as u8;
            }
        }
        self.bbox = (self.w, self.h, 0, 0);
    }
}

/// Feeds a layer's moves to a `Coverage`, joining consecutive moves and
/// folding ones shorter than a fraction of a pixel into the next, so a
/// dense program costs about as much as the pixels it covers.
pub struct Pen<'a> {
    cov: &'a mut Coverage,
    width: f32,
    /// Last point drawn to, and the latest point not drawn yet.
    drawn: Option<(f32, f32)>,
    pending: Option<(f32, f32)>,
}

impl<'a> Pen<'a> {
    const MERGE: f32 = 0.6;

    pub fn new(cov: &'a mut Coverage, width: f32) -> Self {
        Self { cov, width, drawn: None, pending: None }
    }

    pub fn seg(&mut self, a: (f32, f32), b: (f32, f32)) {
        let pen = self.pending.or(self.drawn);
        let joined = pen.is_some_and(|p| (p.0 - a.0).abs() < 0.05 && (p.1 - a.1).abs() < 0.05);
        if !joined {
            self.flush();
            self.drawn = Some(a);
        }
        let d = self.drawn.expect("pen");
        if (b.0 - d.0).abs() < Self::MERGE && (b.1 - d.1).abs() < Self::MERGE {
            self.pending = Some(b);
        } else {
            self.cov.line(d, b, self.width);
            self.drawn = Some(b);
            self.pending = None;
        }
    }

    fn flush(&mut self) {
        if let Some(d) = self.drawn {
            // A lone point still shows as a dot.
            let p = self.pending.take().unwrap_or(d);
            self.cov.line(d, p, self.width);
            self.drawn = Some(p);
        }
    }

    pub fn finish(mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_a_line_and_clears() {
        let mut c = Coverage::new(40, 20);
        c.line((5.0, 10.0), (35.0, 10.0), 1.5);
        c.line((20.0, 2.0), (21.0, 18.0), 1.5);
        let mut pm = Pixmap::new(40, 20).unwrap();
        c.paint(&mut pm, [255, 0, 0], 255);
        let at = |x: u32, y: u32| pm.pixel(x, y).unwrap().alpha();
        assert!(at(20, 10) > 200);
        assert!(at(10, 9) < 255 && at(10, 9) > 0, "{}", at(10, 9));
        assert_eq!(at(10, 4), 0);
        assert!(at(20, 15) > 100);
        assert!(c.buf.iter().all(|&v| v == 0));
    }

    #[test]
    fn far_off_screen_is_cheap_and_empty() {
        let mut c = Coverage::new(10, 10);
        c.line((-1e6, -1e6), (-1e6 + 5.0, -1e6), 1.0);
        assert!(c.bbox.0 >= c.bbox.2);
    }
}
