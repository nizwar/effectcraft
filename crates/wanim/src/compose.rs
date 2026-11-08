//! A reference player: draws one frame the way the format's players do (premultiplied alpha,
//! bilinear sampling inside the sprite's rectangle, `ONE, ONE_MINUS_SRC_ALPHA` blending), on the
//! CPU. The exporter uses it to check each layered frame against the rendered one.

use crate::{Bitmap, Content, Draw, Opened};

/// A premultiplied RGBA float canvas.
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub px: Vec<[f32; 4]>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Canvas {
        Canvas { width, height, px: vec![[0.0; 4]; width as usize * height as usize] }
    }

    /// Straight-alpha 8-bit RGBA.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            let a = p[3].clamp(0.0, 1.0);
            let inv = if a > 0.0 { 1.0 / a } else { 0.0 };
            for c in 0..3 {
                out.push(((p[c] * inv).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
            out.push((a * 255.0 + 0.5) as u8);
        }
        out
    }
}

/// A rectangle of a straight-alpha bitmap, sampled premultiplied.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub bitmap: &'a Bitmap,
    pub rect: [u32; 4],
}

impl Source<'_> {
    /// The premultiplied texel at `(x, y)` of the rectangle, transparent outside it.
    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let [rx, ry, rw, rh] = self.rect;
        if x < 0 || y < 0 || x >= rw as i64 || y >= rh as i64 {
            return [0.0; 4];
        }
        let i = ((ry as usize + y as usize) * self.bitmap.width as usize + rx as usize + x as usize) * 4;
        let Some(p) = self.bitmap.rgba.get(i..i + 4) else { return [0.0; 4] };
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 / 255.0 * a, p[1] as f32 / 255.0 * a, p[2] as f32 / 255.0 * a, a]
    }

    /// Bilinear sample at `(u, v)` in rectangle pixels (texel centres at `n + 0.5`).
    fn sample(&self, u: f64, v: f64) -> [f32; 4] {
        let (fx, fy) = (u - 0.5, v - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) = (self.texel(x0, y0), self.texel(x0 + 1, y0), self.texel(x0, y0 + 1), self.texel(x0 + 1, y0 + 1));
        let mut out = [0f32; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bottom = c[k] + (d[k] - c[k]) * tx;
            out[k] = top + (bottom - top) * ty;
        }
        out
    }
}

/// Draw `src` through `matrix` (`[a, b, c, d, tx, ty]`, rectangle pixels → canvas pixels) with
/// opacity `alpha`, premultiplied over. Pixels whose centre maps inside the rectangle are drawn.
pub fn draw(canvas: &mut Canvas, src: Source, matrix: [f64; 6], alpha: f32) {
    let [a, b, c, d, tx, ty] = matrix;
    let det = a * d - b * c;
    if det.abs() < 1e-12 || !det.is_finite() || alpha <= 0.0 {
        return;
    }
    let (w, h) = (src.rect[2] as f64, src.rect[3] as f64);
    let corners = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(x, y)| (a * x + c * y + tx, b * x + d * y + ty));
    let x_lo = corners.iter().map(|p| p.0).fold(f64::INFINITY, f64::min).floor().max(0.0);
    let x_hi = corners.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max).ceil().min(canvas.width as f64);
    let y_lo = corners.iter().map(|p| p.1).fold(f64::INFINITY, f64::min).floor().max(0.0);
    let y_hi = corners.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max).ceil().min(canvas.height as f64);
    if !(x_lo < x_hi && y_lo < y_hi) {
        return;
    }
    // Inverse: canvas → rectangle.
    let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
    let (itx, ity) = (-(ia * tx + ic * ty), -(ib * tx + id * ty));
    let cw = canvas.width as usize;
    for y in y_lo as usize..y_hi as usize {
        for x in x_lo as usize..x_hi as usize {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let (u, v) = (ia * px + ic * py + itx, ib * px + id * py + ity);
            if !(0.0..w).contains(&u) || !(0.0..h).contains(&v) {
                continue;
            }
            let s = src.sample(u, v);
            let Some(dst) = canvas.px.get_mut(y * cw + x) else { continue };
            let k = 1.0 - s[3] * alpha;
            for ch in 0..4 {
                dst[ch] = s[ch] * alpha + dst[ch] * k;
            }
        }
    }
}

/// Copy `src` 1:1 to `(dx, dy)`, premultiplied over (version 1).
pub fn blit(canvas: &mut Canvas, src: Source, dx: u32, dy: u32) {
    let [a, b, c, d, tx, ty] = [1.0, 0.0, 0.0, 1.0, dx as f64, dy as f64];
    draw(canvas, src, [a, b, c, d, tx, ty], 1.0);
}

/// Matrix of a draw as `f64`s.
pub fn matrix_of(d: &Draw) -> [f64; 6] {
    d.matrix.map(|v| v as f64)
}

/// Draw frame `f` of an opened file.
pub fn frame(file: &Opened, f: usize) -> Canvas {
    let mut canvas = Canvas::new(file.width, file.height);
    match &file.content {
        Content::V1 { sprites, frames } => {
            if let Some(&s) = frames.get(f)
                && s >= 0
                && let Some([atlas, x, y, w, h, dx, dy]) = sprites.get(s as usize).copied()
                && let Some(bitmap) = file.atlases.get(atlas as usize)
            {
                blit(&mut canvas, Source { bitmap, rect: [x, y, w, h] }, dx, dy);
            }
        }
        Content::V2 { sprites, frames } => {
            for d in frames.get(f).map(Vec::as_slice).unwrap_or(&[]) {
                if let Some([atlas, x, y, w, h]) = sprites.get(d.sprite as usize).copied()
                    && let Some(bitmap) = file.atlases.get(atlas as usize)
                {
                    draw(&mut canvas, Source { bitmap, rect: [x, y, w, h] }, matrix_of(d), d.alpha);
                }
            }
        }
    }
    canvas
}
