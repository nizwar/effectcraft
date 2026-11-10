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
//!
//! A version 3 file is planned either way ([`Settings::version`] says how its frames are
//! described), for one or more named frame ranges of the composition ([`build_animations`]).

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

use effectcraft_color::BlendMode;
use effectcraft_geom::{Mat3, vec2};
use effectcraft_project::{ItemId, Quality};
use effectcraft_render::{EvalCtx, Renderer, frame_size};
use effectcraft_time::Tick;

use crate::animations::Clip;
use crate::compose::{Canvas, Source, draw};
use crate::{Animation, Bitmap, Draw, Error, Frames, Placed, Result};

/// What to export.
#[derive(Clone, Debug)]
pub struct Settings {
    /// How frames are described: 1 (one sprite per frame) or 2 (layers). It is also the version
    /// of the file [`crate::write`] makes of the result; [`crate::write_animations`] writes
    /// version 3 from either.
    pub version: u16,
    /// Keep every n-th frame (the file's frame rate is divided by it).
    pub every_nth: u32,
    /// Comp time span, start inclusive, end exclusive. Not used when [`build_animations`] is given
    /// ranges.
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
        // Below half a level of 8-bit alpha the layer draws nothing a player could show.
        if alpha < 0.5 / 255.0 {
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
        // A layer scaled to (nearly) nothing covers no pixel: a draw with a singular matrix would
        // only make players divide by zero (the reference player turns the whole frame to NaN).
        let det = m.determinant().abs();
        if det.is_nan() || det < 1e-4 {
            continue;
        }
        // A layer shrunk below half size is drawn from a box-filtered level, as the compositor
        // samples it (players sample bilinear only, so the level goes into the sprite).
        let level = effectcraft_raster::minified(&st.body.img, m);
        let (img, m) = match &level {
            Some((lvl, f)) => (lvl, *m * Mat3::scale(vec2(1.0 / f, 1.0 / f))),
            None => (&st.body.img, *m),
        };
        let rgba = img.to_rgba8();
        let Some(rect) = content_rect(img.width, img.height, &rgba) else { continue };
        let bitmap = crop(img.width, &rgba, rect, 1);
        // Sprite pixel (1, 1) is buffer pixel (x0, y0).
        let m = m * Mat3::translate(vec2(rect.0 as f64 - 1.0, rect.1 as f64 - 1.0));
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

/// A named frame range of the composition: one animation of a version 3 file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range {
    /// 1 to 64 of `A-Z a-z 0-9 _ . -`, unique in the file.
    pub key: String,
    pub looped: bool,
    /// Composition frames, both inclusive (frame 0 starts at comp time 0).
    pub first: i64,
    pub last: i64,
}

/// How one composition frame is stored, kept so that a frame two animations show is planned once.
#[derive(Clone)]
struct Planned {
    sprite: Option<Placed>,
    draws: Vec<Draw>,
    empty: bool,
    opaque: bool,
    /// Version 2 planning: stored as layers (`Ok`) or as a picture and why.
    layered: Option<std::result::Result<(), String>>,
}

fn check_version(s: &Settings) -> Result<()> {
    if s.version != 1 && s.version != 2 {
        return Err(Error::Format(format!("version {} does not exist: use 1 or 2", s.version)));
    }
    Ok(())
}

fn check_total(total: usize) -> Result<()> {
    if total == 0 {
        return Err(Error::Format("the time span holds no frames".into()));
    }
    if total > 100_000 {
        return Err(Error::TooLarge(format!("{total} frames are more than a .wanim file should hold: export a shorter span or every n-th frame")));
    }
    Ok(())
}

/// Render the composition frames `numbers` (in file order; a repeated number is planned once) at
/// the renderer's scale. `nth` only sets the file's frame rate.
fn plan_frames(
    r: &Renderer,
    comp: ItemId,
    s: &Settings,
    numbers: &[i64],
    nth: i64,
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<(Animation, Report)> {
    let c = r.project.comp(comp).ok_or_else(|| Error::Format("composition not found".into()))?;
    let rate = c.frame_rate;
    let total = numbers.len();
    let size = frame_size(c.width, c.height, r.opts.scale);
    let mut store = Store::default();
    let mut report = Report { frames: total, ..Default::default() };
    let (mut sprites, mut layers) = (vec![], vec![]);
    let mut opaque = true;
    let mut known: HashMap<i64, Planned> = HashMap::new();
    for (i, &number) in numbers.iter().enumerate() {
        if !progress(i, total) {
            return Err(Error::Cancelled);
        }
        let planned = match known.get(&number) {
            Some(p) => p.clone(),
            None => {
                let t = rate.tick_of(number);
                let img = r.comp_frame(comp, t);
                if (img.width, img.height) != size {
                    return Err(Error::Format("the rendered frame has an unexpected size".into()));
                }
                let rendered = img.to_rgba8();
                let mut p = Planned {
                    sprite: None,
                    draws: vec![],
                    empty: false,
                    opaque: rendered.as_chunks::<4>().0.iter().all(|p| p.get(3) == Some(&255)),
                    layered: None,
                };
                let rect = content_rect(size.0, size.1, &rendered);
                p.empty = rect.is_none();
                if s.version == 1 {
                    p.sprite = rect.map(|rc| Placed { bitmap: store.add(crop(size.0, &rendered, rc, 0)), dx: rc.0, dy: rc.1 });
                } else {
                    let ctx = r.eval_ctx(comp, t).ok_or_else(|| Error::Format("composition not found".into()))?;
                    match layered_frame(r, &ctx, size, &rendered, s.tolerance, &mut store) {
                        Ok(draws) => {
                            // Players draw an opaque file without blending, which only a frame
                            // that is a single opaque picture survives.
                            p.opaque &= draws.len() == 1 && draws.iter().all(|d| d.alpha >= 1.0);
                            p.draws = draws;
                            p.layered = Some(Ok(()));
                        }
                        Err(reason) => {
                            p.layered = Some(Err(reason));
                            p.draws = rect.map_or_else(Vec::new, |rc| {
                                let m = [1.0, 0.0, 0.0, 1.0, rc.0 as f32 - 1.0, rc.1 as f32 - 1.0];
                                vec![Draw { sprite: store.add(crop(size.0, &rendered, rc, 1)), matrix: m, alpha: 1.0 }]
                            });
                        }
                    }
                }
                known.insert(number, p.clone());
                p
            }
        };
        opaque &= planned.opaque;
        report.empty += usize::from(planned.empty);
        match planned.layered {
            Some(Ok(())) => report.layered += 1,
            Some(Err(reason)) => *report.flattened.entry(reason).or_default() += 1,
            None => {}
        }
        if s.version == 1 {
            sprites.push(planned.sprite);
        } else {
            layers.push(planned.draws);
        }
    }
    progress(total, total);
    let frames = if s.version == 1 { Frames::Sprites(sprites) } else { Frames::Layers(layers) };
    let name = r.project.item(comp).map(|it| it.name.clone());
    let fps = rate.as_f64() / nth as f64;
    Ok((Animation { width: size.0, height: size.1, fps, composition_id: name, opaque, bitmaps: store.bitmaps, frames }, report))
}

/// The composition frames of the span `s.start..s.end`, every `nth`.
fn span_frames(r: &Renderer, comp: ItemId, s: &Settings, nth: i64) -> Result<Vec<i64>> {
    let c = r.project.comp(comp).ok_or_else(|| Error::Format("composition not found".into()))?;
    let rate = c.frame_rate;
    let (first, end) = (rate.frame_at(s.start), rate.frame_at(s.end.max(s.start)));
    let total = usize::try_from((end - first).max(0).div_euclid(nth) + i64::from((end - first).max(0) % nth != 0)).unwrap_or(0);
    check_total(total)?;
    Ok((0..total as i64).map(|i| first + i * nth).collect())
}

/// Render `comp` at the renderer's scale and describe it for [`crate::write`]. `progress` is
/// called with (frames done, total) and stops the export by returning `false`.
pub fn build(r: &Renderer, comp: ItemId, s: &Settings, progress: &mut dyn FnMut(usize, usize) -> bool) -> Result<(Animation, Report)> {
    r.project.comp(comp).ok_or_else(|| Error::Format("composition not found".into()))?;
    check_version(s)?;
    let nth = s.every_nth.max(1) as i64;
    let numbers = span_frames(r, comp, s, nth)?;
    plan_frames(r, comp, s, &numbers, nth, progress)
}

/// Render `comp` for [`crate::write_animations`]: one animation per range, in the order given,
/// each keeping every n-th frame of its range from its first. Without ranges the file has one
/// animation, `main`, repeating, over the span of the settings. Keys and ranges are checked
/// before anything is rendered; a frame that two ranges share is rendered once.
pub fn build_animations(
    r: &Renderer,
    comp: ItemId,
    s: &Settings,
    ranges: &[Range],
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<(Animation, Vec<Clip>, Report)> {
    let c = r.project.comp(comp).ok_or_else(|| Error::Format("composition not found".into()))?;
    check_version(s)?;
    let nth = s.every_nth.max(1) as i64;
    if ranges.is_empty() {
        let numbers = span_frames(r, comp, s, nth)?;
        let (anim, report) = plan_frames(r, comp, s, &numbers, nth, progress)?;
        return Ok((anim, vec![Clip::main(numbers.len())], report));
    }
    crate::animations::check_keys(ranges.iter().map(|a| a.key.as_str()))?;
    let comp_frames = c.frame_rate.frame_at(c.duration).max(0);
    let mut numbers = vec![];
    let mut clips = Vec::with_capacity(ranges.len().min(1024));
    for a in ranges {
        if a.first < 0 || a.last < a.first || a.last >= comp_frames {
            return Err(Error::Format(format!(
                "the animation `{}` is frames {} to {}, which is not a range inside the composition (frames 0 to {})",
                a.key,
                a.first,
                a.last,
                comp_frames - 1
            )));
        }
        let count = usize::try_from((a.last - a.first) / nth + 1).unwrap_or(usize::MAX);
        if count.saturating_add(numbers.len()) > 100_000 {
            check_total(count.saturating_add(numbers.len()))?;
        }
        numbers.extend((0..count as i64).map(|i| a.first + i * nth));
        clips.push(Clip { key: a.key.clone(), looped: a.looped, frames: count });
    }
    let (anim, report) = plan_frames(r, comp, s, &numbers, nth, progress)?;
    Ok((anim, clips, report))
}
