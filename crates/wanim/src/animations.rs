//! Version 3: several animations in one file, played by key.
//!
//! The animations draw from the same sprites, and the file stores every different draw and every
//! different frame once: a frame is an index into the lists, a list is a run of indices into the
//! draws. A layer that stands still costs one draw and one list however long it is shown, and a
//! picture two animations show is stored once.
//!
//! Atlases are grouped by the animations that draw their sprites, so a player can drop the
//! textures of an animation it no longer plays. Sprites touch on the atlas: each one's own
//! 1-pixel transparent border keeps its pixels from its neighbours'.
//!
//! Both ways of describing frames ([`Frames::Sprites`] and [`Frames::Layers`]) are written by the
//! same writer: a frame that is one picture is a frame with one draw, a translation.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Value, json};

use crate::atlas::{self, AtlasFormat, MAX_ATLAS, Page, Place};
use crate::container::{self, ANIMATIONS_VERSION, Scene};
use crate::{Animation, Bitmap, Draw, Error, Frames, Key, Nonces, Result, Written};

/// The key of the only animation of a file that was exported without keys.
pub const DEFAULT_KEY: &str = "main";
/// The longest animation key.
pub const MAX_KEY: usize = 64;
/// The atlas side used unless a sprite needs more or the caller names one.
const DEFAULT_ATLAS: u32 = 2048;
/// With more groups of animations than this, groups of few pixels share atlases.
const MAX_GROUPS: usize = 8;
const MIN_GROUP_PIXELS: u64 = 128 * 128;
/// A draw fainter than this changes no pixel.
const MIN_ALPHA: f32 = 0.5 / 255.0;

/// One animation of a version 3 file: the next `frames` frames of the [`Animation`] it is written
/// from (animations follow each other, in the order given).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    /// What the app plays the animation by: 1 to 64 of `A-Z a-z 0-9 _ . -`, unique in the file.
    pub key: String,
    /// Whether the animation is meant to repeat.
    pub looped: bool,
    pub frames: usize,
}

impl Clip {
    /// The one animation of a file exported without keys: `main`, repeating.
    pub fn main(frames: usize) -> Clip {
        Clip { key: DEFAULT_KEY.into(), looped: true, frames }
    }
}

pub struct AnimationsOptions {
    pub key: Key,
    /// Largest atlas side, at most [`MAX_ATLAS`]. `None`: 2048, or 4096 when a sprite is larger
    /// than 1024.
    pub atlas_size: Option<u32>,
    pub atlas_format: AtlasFormat,
    pub nonces: Nonces,
}

/// Whether `key` can name an animation.
pub fn check_key(key: &str) -> Result<()> {
    let ok = (1..=MAX_KEY).contains(&key.len()) && key.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'));
    if ok {
        return Ok(());
    }
    let shown: String = key.chars().take(80).collect();
    Err(Error::Format(format!("an animation key is 1 to {MAX_KEY} letters, digits, `_`, `.` or `-`, got `{shown}`")))
}

/// Keys are valid and unique.
pub fn check_keys<'a>(keys: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for key in keys {
        check_key(key)?;
        if !seen.insert(key) {
            return Err(Error::Format(format!("the animation key `{key}` is used twice")));
        }
    }
    if seen.is_empty() {
        return Err(Error::Format("a version 3 file needs at least one animation".into()));
    }
    Ok(())
}

/// A picture as it goes on an atlas: a bitmap of the animation and whether the writer adds its
/// 1-pixel border (frames stored as cropped pictures have none yet).
struct Picture<'a> {
    bitmap: &'a Bitmap,
    /// Atlas size, border included.
    size: (u32, u32),
    /// The animations that draw it.
    users: BTreeSet<u32>,
    /// Every pixel inside the border is fully opaque.
    opaque: bool,
}

/// The draws of every frame over the animation's bitmaps, with matrices that include the sprite's
/// border: frames that are one picture become one translation, and draws that would change no
/// pixel (a singular matrix, no opacity, outside the canvas) are left out.
fn frame_draws(anim: &Animation) -> Result<Vec<Vec<Draw>>> {
    let missing = |b: u32| Error::Format(format!("frame uses bitmap {b}, which does not exist"));
    match &anim.frames {
        Frames::Sprites(frames) => frames
            .iter()
            .map(|f| {
                let Some(p) = f else { return Ok(vec![]) };
                anim.bitmaps.get(p.bitmap as usize).ok_or_else(|| missing(p.bitmap))?;
                Ok(vec![Draw { sprite: p.bitmap, matrix: [1.0, 0.0, 0.0, 1.0, p.dx as f32 - 1.0, p.dy as f32 - 1.0], alpha: 1.0 }])
            })
            .collect(),
        Frames::Layers(frames) => frames
            .iter()
            .map(|f| {
                let mut out = Vec::with_capacity(f.len());
                for d in f {
                    let bm = anim.bitmaps.get(d.sprite as usize).ok_or_else(|| missing(d.sprite))?;
                    if !d.matrix.iter().all(|v| v.is_finite()) || !d.alpha.is_finite() {
                        return Err(Error::Format("a draw has a matrix or opacity that is not a number".into()));
                    }
                    let [a, b, c, dd, tx, ty] = d.matrix;
                    let det = (a * dd - b * c).abs();
                    if det.is_nan() || det <= 1e-9 || d.alpha < MIN_ALPHA {
                        continue;
                    }
                    // The picture inside the border, on the canvas.
                    let (w, h) = (bm.width as f32 - 1.0, bm.height as f32 - 1.0);
                    let corners = [(1.0, 1.0), (w, 1.0), (1.0, h), (w, h)].map(|(x, y)| (a * x + c * y + tx, b * x + dd * y + ty));
                    let (x_lo, x_hi) = corners.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |m, p| (m.0.min(p.0), m.1.max(p.0)));
                    let (y_lo, y_hi) = corners.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |m, p| (m.0.min(p.1), m.1.max(p.1)));
                    if !(x_hi > 0.0 && x_lo < anim.width as f32 && y_hi > 0.0 && y_lo < anim.height as f32) {
                        continue;
                    }
                    // -0 and 0 are the same number: one bit pattern, so equal draws compare equal.
                    out.push(Draw { sprite: d.sprite, matrix: d.matrix.map(|v| v + 0.0), alpha: d.alpha.clamp(0.0, 1.0) });
                }
                Ok(out)
            })
            .collect(),
    }
}

/// Pictures with the same users share atlases: `(users, pictures)` in the order the groups are
/// first drawn. With many groups, those of few pixels are put together at the end.
fn groups(pictures: &[Picture]) -> Vec<Vec<usize>> {
    let mut index: HashMap<&BTreeSet<u32>, usize> = HashMap::new();
    let mut out: Vec<Vec<usize>> = vec![];
    for (i, p) in pictures.iter().enumerate() {
        let at = *index.entry(&p.users).or_insert_with(|| {
            out.push(vec![]);
            out.len() - 1
        });
        if let Some(g) = out.get_mut(at) {
            g.push(i);
        }
    }
    if out.len() > MAX_GROUPS {
        let area = |g: &Vec<usize>| -> u64 { g.iter().filter_map(|&i| pictures.get(i)).map(|p| p.size.0 as u64 * p.size.1 as u64).sum() };
        let (mut kept, mut small) = (vec![], vec![]);
        for g in out {
            if area(&g) < MIN_GROUP_PIXELS {
                small.extend(g);
            } else {
                kept.push(g);
            }
        }
        if !small.is_empty() {
            small.sort_unstable();
            kept.push(small);
        }
        out = kept;
    }
    out
}

/// Copy the pictures into their pages (transparent elsewhere) and encode each page.
fn render(pages: &[Page], places: &[Place], pictures: &[Picture], border: u32, format: AtlasFormat) -> Result<Vec<Vec<u8>>> {
    let mut pixels: Vec<Vec<u8>> = pages.iter().map(|p| vec![0u8; p.width as usize * p.height as usize * 4]).collect();
    for (place, pic) in places.iter().zip(pictures) {
        let page = pages.get(place.page as usize).ok_or_else(|| Error::Format("sprite on a missing page".into()))?;
        let dst = pixels.get_mut(place.page as usize).ok_or_else(|| Error::Format("sprite on a missing page".into()))?;
        let row = pic.bitmap.width as usize * 4;
        for y in 0..pic.bitmap.height as usize {
            let src = pic.bitmap.rgba.get(y * row..(y + 1) * row).ok_or_else(|| Error::Format("bitmap is shorter than its size".into()))?;
            let at = ((place.y as usize + border as usize + y) * page.width as usize + place.x as usize + border as usize) * 4;
            dst.get_mut(at..at + row).ok_or_else(|| Error::Format("sprite runs past its atlas".into()))?.copy_from_slice(src);
        }
    }
    pages.iter().zip(pixels).map(|(p, px)| atlas::encode(format, p.width, p.height, &px)).collect()
}

/// Pack and encrypt `anim` as a version 3 file with the animations `clips`, which take its frames
/// in order.
pub fn write_animations(anim: &Animation, clips: &[Clip], opts: &AnimationsOptions) -> Result<Written> {
    crate::check_canvas(anim)?;
    check_keys(clips.iter().map(|c| c.key.as_str()))?;
    if let Some(c) = clips.iter().find(|c| c.frames == 0) {
        return Err(Error::Format(format!("the animation `{}` has no frames", c.key)));
    }
    let total = clips.iter().try_fold(0usize, |n, c| n.checked_add(c.frames)).ok_or_else(|| Error::TooLarge("too many frames".into()))?;
    if total != anim.frame_count() {
        return Err(Error::Format(format!("the animations take {total} frames, but {} were given", anim.frame_count())));
    }
    if opts.atlas_size.is_some_and(|s| s == 0 || s > MAX_ATLAS) {
        return Err(Error::Format(format!("the atlas size is 1 to {MAX_ATLAS}")));
    }
    let many = || Error::TooLarge("too many sprites, draws or frames".into());
    let frames = frame_draws(anim)?;
    // Sprites drawn 1:1 from cropped pictures get their border here; layer sprites carry it.
    let border = u32::from(matches!(anim.frames, Frames::Sprites(_)));

    // Pictures in the order they are first drawn, so atlases are in playback order. Bitmaps with
    // the same pixels are one picture.
    let mut pictures: Vec<Picture> = vec![];
    let mut of_bitmap: Vec<Option<u32>> = vec![None; anim.bitmaps.len()];
    let mut of_pixels: HashMap<&Bitmap, u32> = HashMap::new();
    let mut frame = frames.iter();
    for (clip, c) in clips.iter().enumerate() {
        let clip = u32::try_from(clip).map_err(|_| many())?;
        for draws in frame.by_ref().take(c.frames) {
            for d in draws {
                let slot = of_bitmap.get_mut(d.sprite as usize).ok_or_else(|| Error::Format("unpacked bitmap".into()))?;
                let picture = match *slot {
                    Some(p) => p,
                    None => {
                        let bm = anim.bitmaps.get(d.sprite as usize).ok_or_else(|| Error::Format("unpacked bitmap".into()))?;
                        if bm.width == 0 || bm.height == 0 || bm.rgba.len() as u64 != bm.width as u64 * bm.height as u64 * 4 {
                            return Err(Error::Format("a bitmap is not as large as its size says".into()));
                        }
                        let next = u32::try_from(pictures.len()).map_err(|_| many())?;
                        let p = *of_pixels.entry(bm).or_insert(next);
                        if p == next {
                            let size = (bm.width.saturating_add(2 * border), bm.height.saturating_add(2 * border));
                            // The pixels inside the border (a layer sprite's own one).
                            let inset = 1 - border;
                            let opaque = bm.width > 2 * inset
                                && bm.height > 2 * inset
                                && bm.rgba.as_chunks::<4>().0.iter().enumerate().all(|(i, px)| {
                                    let (x, y) = ((i % bm.width as usize) as u32, (i / bm.width as usize) as u32);
                                    px[3] == 255 || x < inset || y < inset || x >= bm.width - inset || y >= bm.height - inset
                                });
                            pictures.push(Picture { bitmap: bm, size, users: BTreeSet::new(), opaque });
                        }
                        *slot = Some(p);
                        p
                    }
                };
                if let Some(p) = pictures.get_mut(picture as usize) {
                    p.users.insert(clip);
                }
            }
        }
    }

    let sizes: Vec<(u32, u32)> = pictures.iter().map(|p| p.size).collect();
    let largest = sizes.iter().map(|s| s.0.max(s.1)).max().unwrap_or(0);
    let page_size = opts.atlas_size.unwrap_or(if largest > DEFAULT_ATLAS / 2 { MAX_ATLAS } else { DEFAULT_ATLAS });
    let mut pages: Vec<Page> = vec![];
    let mut places = vec![Place { page: 0, x: 0, y: 0 }; pictures.len()];
    for group in groups(&pictures) {
        let group_sizes: Vec<(u32, u32)> = group.iter().filter_map(|&i| sizes.get(i).copied()).collect();
        let (group_pages, group_places) = atlas::pack_sprites(&group_sizes, page_size)?;
        let first = u32::try_from(pages.len()).map_err(|_| many())?;
        for (&i, place) in group.iter().zip(group_places) {
            if let Some(p) = places.get_mut(i) {
                *p = Place { page: first.checked_add(place.page).ok_or_else(many)?, ..place };
            }
        }
        pages.extend(group_pages);
    }
    let mut blocks = render(&pages, &places, &pictures, border, opts.atlas_format)?;
    let sprites: Vec<[u32; 5]> = places.iter().zip(&sizes).map(|(p, s)| [p.page, p.x, p.y, s.0, s.1]).collect();

    // Every different draw and every different frame once.
    let mut scene = Scene::default();
    let mut draw_of: HashMap<[u32; 8], u32> = HashMap::new();
    let mut list_of: HashMap<Vec<u32>, u32> = HashMap::new();
    for draws in &frames {
        let mut list = Vec::with_capacity(draws.len());
        for d in draws {
            let sprite = of_bitmap.get(d.sprite as usize).copied().flatten().ok_or_else(|| Error::Format("unpacked bitmap".into()))?;
            let m = d.matrix.map(f32::to_bits);
            let id = [sprite, m[0], m[1], m[2], m[3], m[4], m[5], d.alpha.to_bits()];
            let next = u32::try_from(scene.draws.len()).map_err(|_| many())?;
            let index = *draw_of.entry(id).or_insert(next);
            if index == next {
                scene.draws.push(Draw { sprite, ..*d });
            }
            list.push(index);
        }
        let next = u32::try_from(scene.lists.len()).map_err(|_| many())?;
        let index = match list_of.get(&list) {
            Some(&i) => i,
            None => {
                list_of.insert(list.clone(), next);
                scene.lists.push(list);
                next
            }
        };
        scene.frames.push(index);
    }
    let (scene_bytes, items) = container::encode_scene(&scene)?;
    blocks.push(container::deflate(&scene_bytes));

    // Blending can only be turned off if every frame is one opaque picture covering the canvas.
    let opaque = frames.iter().all(|draws| {
        let [d] = draws.as_slice() else { return false };
        let Some(p) = of_bitmap.get(d.sprite as usize).copied().flatten().and_then(|p| pictures.get(p as usize)) else { return false };
        // The matrix of the picture inside the border.
        let [a, b, c, dd, tx, ty] = d.matrix;
        let inner = [a, b, c, dd, a + c + tx, b + dd + ty];
        p.opaque
            && d.alpha >= 1.0
            && inner.iter().zip([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]).all(|(v, i)| (v - i).abs() < 1e-6)
            && (p.size.0, p.size.1) == (anim.width.saturating_add(2), anim.height.saturating_add(2))
    });

    let gpu_bytes: u64 = pages.iter().map(|p| p.width as u64 * p.height as u64 * 4).sum();
    let animations: Vec<Value> = clips
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let used: BTreeSet<u32> = pictures.iter().zip(&places).filter(|(p, _)| p.users.contains(&(i as u32))).map(|(_, place)| place.page).collect();
            json!({"key": c.key, "frameCount": c.frames, "loop": c.looped, "atlases": used})
        })
        .collect();
    let n_pages = pages.len();
    let (file, manifest) = container::write_file(ANIMATIONS_VERSION, &opts.key, &opts.nonces, &blocks, |spans| {
        let atlases: Vec<Value> = pages.iter().zip(spans).map(|(p, s)| container::atlas_entry(*s, p.width, p.height, opts.atlas_format)).collect();
        let (offset, length) = spans.get(n_pages).copied().unwrap_or((0, 0));
        json!({
            "version": ANIMATIONS_VERSION,
            "compositionId": anim.composition_id,
            "width": anim.width,
            "height": anim.height,
            "fps": anim.fps,
            "frameCount": total,
            "opaque": opaque,
            "gpuBytes": gpu_bytes,
            "atlases": atlases,
            "sprites": sprites,
            "animations": animations,
            "scene": {"offset": offset, "length": length, "draws": scene.draws.len(), "lists": scene.lists.len(), "items": items},
        })
    })?;
    Ok(Written { file, manifest, frame_count: total, sprites: pictures.len(), atlases: n_pages, draws: scene.draws.len(), gpu_bytes })
}
