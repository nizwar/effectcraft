//! Turning a composition into an [`Animation`].
//!
//! Every frame is rendered as usual. For version 1 that picture, cropped to its pixels, is the
//! frame's sprite. For version 2 the exporter first tries to describe the frame as layers: each
//! visible layer's processed pixels (source, masks and effects, before its transform; the same
//! buffer the compositor caches) become a sprite, drawn with the layer's transform as a matrix and
//! its opacity. A layer that only moves, turns, scales or fades keeps the same pixels, so it is
//! stored once. The layered frame is then drawn with the reference player ([`crate::compose`])
//! and compared with the rendered picture; if they differ, or the frame holds something a list of
//! transformed sprites can't express (3D layers, blend modes other than Normal, track mattes,
//! adjustment layers, layer styles, motion blur, collapsed precomps), the frame is stored as one
//! picture instead. A layered file therefore always looks like the rendered composition.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

use effectcraft_color::BlendMode;
use effectcraft_geom::{Mat3, vec2};
use effectcraft_project::{ItemId, Quality};
use effectcraft_render::{EvalCtx, Renderer, frame_size};
use effectcraft_time::Tick;

use crate::compose::{Canvas, Source, draw};
use crate::{Animation, Bitmap, Draw, Error, Frames, Placed, Result};

/// What to export.
#[derive(Clone, Debug)]
pub struct Settings {
    /// 1 (one sprite per frame) or 2 (layers).
    pub version: u16,
    /// Keep every n-th frame (the file's frame rate is divided by it).
    pub every_nth: u32,
    /// Comp time span, start inclusive, end exclusive.
    pub start: Tick,
    pub end: Tick,
    /// How far a layered frame may be from the rendered one (see [`Tolerance`]).
    pub tolerance: Tolerance,
}

/// A layered frame is used when at most `share` of its pixels differ from the rendered frame by
/// more than `level` (0–255, premultiplied channels), and none by more than `max`.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    pub level: u8,
    pub share: f64,
    pub max: u8,
}

impl Default for Tolerance {
    fn default() -> Self {
        Tolerance { level: 6, share: 0.002, max: 40 }
    }
}

/// How the frames were stored.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub frames: usize,
    /// Version 2: frames stored as layers.
    pub layered: usize,
    /// Frames stored as one picture, and why (version 2), e.g. `"3D layer": 12`.
    pub flattened: BTreeMap<String, usize>,
    pub empty: usize,
}

/// Bitmaps kept once, by content.
#[derive(Default)]
struct Store {
    bitmaps: Vec<Bitmap>,
    index: HashMap<u64, Vec<u32>>,
}

impl Store {
    fn add(&mut self, b: Bitmap) -> u32 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        b.hash(&mut h);
        let key = h.finish();
        let same = self.index.entry(key).or_default();
        if let Some(&i) = same.iter().find(|&&i| self.bitmaps.get(i as usize) == Some(&b)) {
            return i;
        }
        let i = self.bitmaps.len() as u32;
        same.push(i);
        self.bitmaps.push(b);
        i
    }
}

/// The smallest rectangle holding every pixel with alpha > 0: `(x0, y0, w, h)`.
fn content_rect(width: u32, height: u32, rgba: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (i, p) in rgba.as_chunks::<4>().0.iter().enumerate() {
        if p.get(3).copied().unwrap_or(0) == 0 {
            continue;
        }
        let (x, y) = ((i % width as usize) as u32, (i / width as usize) as u32);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    (x0 <= x1 && y0 <= y1 && x1 < width && y1 < height).then(|| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

/// Crop `rgba` to `rect`, with a `border`-pixel transparent frame around it.
fn crop(width: u32, rgba: &[u8], rect: (u32, u32, u32, u32), border: u32) -> Bitmap {
    let (x0, y0, w, h) = rect;
    let (bw, bh) = (w + 2 * border, h + 2 * border);
    let mut out = vec![0u8; bw as usize * bh as usize * 4];
    for y in 0..h as usize {
        let src = (y0 as usize + y) * width as usize * 4 + x0 as usize * 4;
        let dst = ((y + border as usize) * bw as usize + border as usize) * 4;
        if let (Some(s), Some(d)) = (rgba.get(src..src + w as usize * 4), out.get_mut(dst..dst + w as usize * 4)) {
            d.copy_from_slice(s);
        }
    }
    Bitmap { width: bw, height: bh, rgba: out }
}

/// A sprite to be drawn: its pixels (with the 1-pixel border), matrix and opacity.
struct Candidate {
    bitmap: Bitmap,
    matrix: [f64; 6],
    alpha: f32,
}

fn to_array(m: &Mat3) -> [f64; 6] {
    let r = &m.0;
    [r[0][0], r[1][0], r[0][1], r[1][1], r[0][2], r[1][2]]
}

/// Why the frame can't be layers, or its sprites.
fn layer_candidates(r: &Renderer, ctx: &EvalCtx) -> std::result::Result<Vec<Candidate>, &'static str> {
    if ctx.comp.layers.iter().any(|l| l.track_matte.is_some()) {
        return Err("track matte");
    }
    let mut out = vec![];
    for layer in r.visible_layers(ctx) {
        if layer.is_3d() {
            return Err("3D layer");
        }
        if layer.switches.adjustment {
            return Err("adjustment layer");
        }
        if r.quality(layer) == Quality::Wireframe {
            return Err("wireframe layer");
        }
        if layer.blend_mode != BlendMode::Normal {
            return Err("blend mode");
        }
        if layer.preserve_transparency {
            return Err("preserve transparency");
        }
        if r.collapsed(ctx, layer).is_some() {
            return Err("collapsed precomp");
        }
        let alpha = r.layer_opacity(ctx, layer);
        if alpha <= 0.0 {
            continue;
        }
        let Some(st) = r.styled_layer(ctx, layer) else { continue };
        if !st.plain {
            return Err("layer styles");
        }
        let placement = r.placement(ctx, layer, &st.body);
        let [m] = placement.matrices.as_slice() else { return Err("motion blur") };
        if !m.is_affine() {
            return Err("perspective");
        }
        let img = &st.body.img;
        let rgba = img.to_rgba8();
        let Some(rect) = content_rect(img.width, img.height, &rgba) else { continue };
        let bitmap = crop(img.width, &rgba, rect, 1);
        // Sprite pixel (1, 1) is buffer pixel (x0, y0).
        let m = *m * Mat3::translate(vec2(rect.0 as f64 - 1.0, rect.1 as f64 - 1.0));
        out.push(Candidate { bitmap, matrix: to_array(&m), alpha: alpha.min(1.0) });
    }
    Ok(out)
}

/// Whether the layered frame matches the rendered one.
fn matches(layered: &[u8], rendered: &[u8], tol: Tolerance) -> bool {
    let premul = |p: &[u8], c: usize| -> i32 { (p.get(c).copied().unwrap_or(0) as i32 * p.get(3).copied().unwrap_or(0) as i32 + 127) / 255 };
    let (mut off, mut n) = (0usize, 0usize);
    for (a, b) in layered.as_chunks::<4>().0.iter().zip(rendered.as_chunks::<4>().0.iter()) {
        n += 1;
        let worst = (0..4).map(|c| if c == 3 { (a[3] as i32 - b[3] as i32).abs() } else { (premul(a, c) - premul(b, c)).abs() }).max().unwrap_or(0);
        if worst > tol.max as i32 {
            return false;
        }
        if worst > tol.level as i32 {
            off += 1;
        }
    }
    n > 0 && (off as f64) <= tol.share * n as f64
}

fn draw_candidates(width: u32, height: u32, c: &[Candidate]) -> Vec<u8> {
    let mut canvas = Canvas::new(width, height);
    for k in c {
        let src = Source { bitmap: &k.bitmap, rect: [0, 0, k.bitmap.width, k.bitmap.height] };
        draw(&mut canvas, src, k.matrix, k.alpha);
    }
    canvas.to_rgba8()
}

fn f32s(m: [f64; 6]) -> [f32; 6] {
    m.map(|v| v as f32)
}

/// One frame of a version 2 file: its draws, or `Err(reason)` when it was stored as a picture.
fn layered_frame(r: &Renderer, ctx: &EvalCtx, size: (u32, u32), rendered: &[u8], tol: Tolerance, store: &mut Store) -> std::result::Result<Vec<Draw>, String> {
    let candidates = layer_candidates(r, ctx).map_err(str::to_string)?;
    if !matches(&draw_candidates(size.0, size.1, &candidates), rendered, tol) {
        return Err("differs from the rendered frame".into());
    }
    Ok(candidates.into_iter().map(|c| Draw { sprite: store.add(c.bitmap), matrix: f32s(c.matrix), alpha: c.alpha }).collect())
}

/// Render `comp` at the renderer's scale and describe it for [`crate::write`]. `progress` is
/// called with (frames done, total) and stops the export by returning `false`.
pub fn build(r: &Renderer, comp: ItemId, s: &Settings, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<(Animation, Report)> {
    let c = r.project.comp(comp).ok_or_else(|| Error::Format("composition not found".into()))?;
    if s.version != 1 && s.version != 2 {
        return Err(Error::Format(format!("version {} does not exist: use 1 or 2", s.version)));
    }
    let rate = c.frame_rate;
    let nth = s.every_nth.max(1) as i64;
    let (first, end) = (rate.frame_at(s.start), rate.frame_at(s.end.max(s.start)));
    let total = usize::try_from((end - first).max(0).div_euclid(nth) + i64::from((end - first).max(0) % nth != 0)).unwrap_or(0);
    if total == 0 {
        return Err(Error::Format("the time span holds no frames".into()));
    }
    if total > 100_000 {
        return Err(Error::TooLarge(format!("{total} frames are more than a .wanim file should hold: export a shorter span or every n-th frame")));
    }
    let size = frame_size(c.width, c.height, r.opts.scale);
    let mut store = Store::default();
    let mut report = Report { frames: total, ..Default::default() };
    let (mut sprites, mut layers) = (vec![], vec![]);
    let mut opaque = true;
    for i in 0..total {
        if !progress(i, total) {
            return Err(Error::Cancelled);
        }
        let t = rate.tick_of(first + i as i64 * nth);
        let img = r.comp_frame(comp, t);
        if (img.width, img.height) != size {
            return Err(Error::Format("the rendered frame has an unexpected size".into()));
        }
        let rendered = img.to_rgba8();
        opaque &= rendered.as_chunks::<4>().0.iter().all(|p| p.get(3) == Some(&255));
        let rect = content_rect(size.0, size.1, &rendered);
        if rect.is_none() {
            report.empty += 1;
        }
        if s.version == 1 {
            sprites.push(rect.map(|rc| Placed { bitmap: store.add(crop(size.0, &rendered, rc, 0)), dx: rc.0, dy: rc.1 }));
            continue;
        }
        let ctx = r.eval_ctx(comp, t).ok_or_else(|| Error::Format("composition not found".into()))?;
        match layered_frame(r, &ctx, size, &rendered, s.tolerance, &mut store) {
            Ok(draws) => {
                report.layered += 1;
                // Players draw an opaque file without blending, which only a frame that is a
                // single opaque picture survives.
                opaque &= draws.len() == 1 && draws.iter().all(|d| d.alpha >= 1.0);
                layers.push(draws);
            }
            Err(reason) => {
                *report.flattened.entry(reason).or_default() += 1;
                layers.push(rect.map_or_else(Vec::new, |rc| {
                    let m = [1.0, 0.0, 0.0, 1.0, rc.0 as f32 - 1.0, rc.1 as f32 - 1.0];
                    vec![Draw { sprite: store.add(crop(size.0, &rendered, rc, 1)), matrix: m, alpha: 1.0 }]
                }));
            }
        }
    }
    progress(total, total);
    let frames = if s.version == 1 { Frames::Sprites(sprites) } else { Frames::Layers(layers) };
    let name = r.project.item(comp).map(|it| it.name.clone());
    let fps = rate.as_f64() / nth as f64;
    Ok((Animation { width: size.0, height: size.1, fps, composition_id: name, opaque, bitmaps: store.bitmaps, frames }, report))
}
