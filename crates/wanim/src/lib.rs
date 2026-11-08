//! # effectcraft-wanim (L3)
//!
//! Writes compositions as `.wanim` sprite animations: texture atlases plus a manifest that says
//! what to draw in each frame, encrypted so that only players holding the key can read them.
//!
//! * **Version 1** stores one sprite per frame: the frame, cropped to its pixels, at an offset.
//!   Frames that look the same share a sprite.
//! * **Version 2** stores a list of transformed sprites per frame. A layer that only moves,
//!   turns, scales or fades is stored once and drawn with a matrix and an opacity in every frame;
//!   a layer whose pixels change gets one sprite per look ([`plan`]).
//!
//! ## File layout
//!
//! All integers are little-endian.
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 8 | magic `89 57 41 4E 49 4D 0D 0A` |
//! | 8 | 2 | `u16` version, 1 or 2 |
//! | 10 | 2 | `u16` flags: bit 0 = custom key |
//! | 12 | 4 | `u32` sealed manifest length |
//! | 16 | 16 | salt |
//! | 32 | 8 | nonce prefix |
//! | 40 | … | block 0: the manifest (UTF-8 JSON), then the data section: atlas blocks (PNG), then the version 2 timeline |
//!
//! The file key is HKDF-SHA256 of the master key with the salt and the info
//! `wanim/v1/aes-256-gcm`. Block `i` is AES-256-GCM with the nonce `noncePrefix ‖ u32(i)` and
//! the associated data `header ‖ u32(i)`, its 16-byte tag at the end.
//!
//! The **official key** that players have built in is never part of this repository: the app
//! reads it from the user's settings folder ([`Key::official`]). Files written with any other
//! key set the custom-key flag, and the app playing them has to supply that key.
//!
//! The format follows the `.wanim` specification shipped with the format's packer; files are
//! checked against its conformance fixtures by the tests in this crate when they are available
//! locally (`EFFECTCRAFT_WANIM_FIXTURES`).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod atlas;
pub mod compose;
pub mod container;
pub mod plan;

use serde_json::{Value, json};

pub use container::{Header, Key, Nonces, read_header};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("wanim key: {0}")]
    Key(String),
    #[error("wanim: {0}")]
    Crypto(String),
    #[error("wanim: {0}")]
    Format(String),
    #[error("wanim: {0}")]
    TooLarge(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Straight-alpha 8-bit RGBA pixels, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One version 2 draw: sprite (or, in an [`Animation`], bitmap) index, the matrix
/// `[a, b, c, d, tx, ty]` mapping sprite pixels to canvas pixels
/// (`x' = a·x + c·y + tx`, `y' = b·x + d·y + ty`), and an opacity from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Draw {
    pub sprite: u32,
    pub matrix: [f32; 6],
    pub alpha: f32,
}

/// A version 1 frame: a bitmap copied to `(dx, dy)` on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub bitmap: u32,
    pub dx: u32,
    pub dy: u32,
}

/// What each frame draws.
#[derive(Clone, Debug, PartialEq)]
pub enum Frames {
    /// Version 1: at most one bitmap per frame (`None` = an empty frame).
    Sprites(Vec<Option<Placed>>),
    /// Version 2: draws back to front. Each bitmap must already have its 1-pixel transparent
    /// border, which the matrix includes.
    Layers(Vec<Vec<Draw>>),
}

/// An animation ready to be packed.
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub composition_id: Option<String>,
    /// Every frame covers the whole canvas with opaque pixels.
    pub opaque: bool,
    pub bitmaps: Vec<Bitmap>,
    pub frames: Frames,
}

impl Animation {
    pub fn frame_count(&self) -> usize {
        match &self.frames {
            Frames::Sprites(f) => f.len(),
            Frames::Layers(f) => f.len(),
        }
    }

    pub fn version(&self) -> u16 {
        match self.frames {
            Frames::Sprites(_) => 1,
            Frames::Layers(_) => 2,
        }
    }
}

pub struct WriteOptions {
    pub key: Key,
    /// Largest atlas side, at most [`atlas::MAX_ATLAS`].
    pub atlas_size: u32,
    pub nonces: Nonces,
}

/// A written file and what it holds.
#[derive(Debug)]
pub struct Written {
    pub file: Vec<u8>,
    pub manifest: Value,
    pub frame_count: usize,
    pub sprites: usize,
    pub atlases: usize,
    pub draws: usize,
    /// Memory of all atlases as RGBA8 textures.
    pub gpu_bytes: u64,
}

/// Bitmaps in the order frames first use them: `(order, remap)` where `order[i]` is the bitmap
/// index of packed sprite `i` and `remap[bitmap]` its sprite index (unused bitmaps are dropped).
fn first_use(n: usize, uses: impl Iterator<Item = u32>) -> Result<(Vec<usize>, Vec<Option<u32>>)> {
    let mut remap = vec![None; n];
    let mut order = vec![];
    for b in uses {
        let slot = remap.get_mut(b as usize).ok_or_else(|| Error::Format(format!("frame uses bitmap {b}, which does not exist")))?;
        if slot.is_none() {
            *slot = Some(order.len() as u32);
            order.push(b as usize);
        }
    }
    Ok((order, remap))
}

/// Pack and encrypt an animation.
pub fn write(anim: &Animation, opts: &WriteOptions) -> Result<Written> {
    if anim.width == 0 || anim.height == 0 {
        return Err(Error::Format("the canvas has no pixels".into()));
    }
    if !anim.fps.is_finite() || anim.fps <= 0.0 {
        return Err(Error::Format("the frame rate must be positive".into()));
    }
    let uses: Vec<u32> = match &anim.frames {
        Frames::Sprites(f) => f.iter().flatten().map(|p| p.bitmap).collect(),
        Frames::Layers(f) => f.iter().flatten().map(|d| d.sprite).collect(),
    };
    let (order, remap) = first_use(anim.bitmaps.len(), uses.into_iter())?;
    let bitmaps: Vec<&Bitmap> = order.iter().filter_map(|&i| anim.bitmaps.get(i)).collect();
    let sizes: Vec<(u32, u32)> = bitmaps.iter().map(|b| (b.width, b.height)).collect();
    let (pages, places) = atlas::shelf_pack(&sizes, opts.atlas_size)?;
    let mut blocks = atlas::render_pages(&pages, &places, &bitmaps)?;
    let gpu_bytes: u64 = pages.iter().map(|p| p.width as u64 * p.height as u64 * 4).sum();
    let base = json!({
        "compositionId": anim.composition_id,
        "width": anim.width,
        "height": anim.height,
        "fps": anim.fps,
        "frameCount": anim.frame_count(),
        "opaque": anim.opaque,
        "gpuBytes": gpu_bytes,
    });
    let rect = |s: u32| -> Result<(Place4, &Bitmap)> {
        let p = places.get(s as usize).ok_or_else(|| Error::Format("sprite was not packed".into()))?;
        let b = bitmaps.get(s as usize).ok_or_else(|| Error::Format("sprite was not packed".into()))?;
        Ok(([p.page, p.x, p.y, b.width, b.height], b))
    };
    let n_pages = pages.len();
    let (version, sprites_json, extra, draws) = match &anim.frames {
        Frames::Sprites(frames) => {
            // One sprite per distinct (bitmap, offset); frames point at sprites or -1.
            let mut sprites: Vec<[u32; 7]> = vec![];
            let mut index = std::collections::HashMap::new();
            let mut list = Vec::with_capacity(frames.len());
            for f in frames {
                let Some(p) = f else {
                    list.push(-1i64);
                    continue;
                };
                let s = remap.get(p.bitmap as usize).copied().flatten().ok_or_else(|| Error::Format("unpacked bitmap".into()))?;
                let ([a, x, y, w, h], _) = rect(s)?;
                let key = [a, x, y, w, h, p.dx, p.dy];
                let id = *index.entry(key).or_insert_with(|| {
                    sprites.push(key);
                    sprites.len() - 1
                });
                list.push(id as i64);
            }
            (1u16, json!(sprites), json!({"frames": list}), 0usize)
        }
        Frames::Layers(frames) => {
            let mut out = Vec::with_capacity(frames.len());
            for f in frames {
                let mut v = Vec::with_capacity(f.len());
                for d in f {
                    let sprite = remap.get(d.sprite as usize).copied().flatten().ok_or_else(|| Error::Format("unpacked bitmap".into()))?;
                    if !d.matrix.iter().all(|v| v.is_finite()) || !d.alpha.is_finite() {
                        return Err(Error::Format("a draw has a matrix or opacity that is not a number".into()));
                    }
                    // A draw that covers no area (singular matrix) or is invisible draws nothing;
                    // players would divide by zero inverting its matrix.
                    let [a, b, c, dd, _, _] = d.matrix;
                    let det = (a * dd - b * c).abs();
                    if det.is_nan() || det <= 1e-9 || d.alpha < 0.5 / 255.0 {
                        continue;
                    }
                    v.push(Draw { sprite, matrix: d.matrix, alpha: d.alpha.clamp(0.0, 1.0) });
                }
                out.push(v);
            }
            let sprites: Vec<Place4> = (0..order.len() as u32).map(|s| rect(s).map(|r| r.0)).collect::<Result<_>>()?;
            let draws = out.iter().map(Vec::len).sum();
            blocks.push(container::encode_timeline(&out)?);
            (2u16, json!(sprites), Value::Null, draws)
        }
    };
    let (file, manifest) = container::write_file(version, &opts.key, &opts.nonces, &blocks, |spans| {
        let mut m = json!({"version": version});
        if let (Some(m), Some(b)) = (m.as_object_mut(), base.as_object()) {
            m.extend(b.clone());
            let atlases: Vec<Value> = pages.iter().zip(spans).map(|(p, s)| container::atlas_entry(*s, p.width, p.height)).collect();
            m.insert("atlases".into(), json!(atlases));
            m.insert("sprites".into(), sprites_json);
            if version == 1 {
                if let Some(e) = extra.as_object() {
                    m.extend(e.clone());
                }
            } else if let Some(t) = spans.get(n_pages) {
                m.insert("timeline".into(), json!({"offset": t.0, "length": t.1, "draws": draws}));
            }
        }
        m
    })?;
    Ok(Written { file, manifest, frame_count: anim.frame_count(), sprites: order.len(), atlases: n_pages, draws, gpu_bytes })
}

type Place4 = [u32; 5];

/// What a version 1 or 2 file draws, decoded.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// `[atlas, x, y, w, h, dx, dy]` and, per frame, a sprite index or -1.
    V1 { sprites: Vec<[u32; 7]>, frames: Vec<i64> },
    /// `[atlas, x, y, w, h]` and the draws of each frame.
    V2 { sprites: Vec<[u32; 5]>, frames: Vec<Vec<Draw>> },
}

/// A decrypted, checked file.
#[derive(Clone, Debug)]
pub struct Opened {
    pub header: Header,
    pub manifest: Value,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub opaque: bool,
    pub atlases: Vec<Bitmap>,
    pub content: Content,
}

impl Opened {
    pub fn frame_count(&self) -> usize {
        match &self.content {
            Content::V1 { frames, .. } => frames.len(),
            Content::V2 { frames, .. } => frames.len(),
        }
    }
}

fn u32_field(m: &Value, k: &str) -> Result<u32> {
    m.get(k)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| Error::Format(format!("manifest field `{k}` is missing or out of range")))
}

fn sprite_list<const N: usize>(m: &Value, atlases: &[Bitmap]) -> Result<Vec<[u32; N]>> {
    let list = m.get("sprites").and_then(Value::as_array).ok_or_else(|| Error::Format("manifest has no sprites".into()))?;
    list.iter()
        .map(|s| {
            let a = s.as_array().filter(|a| a.len() == N).ok_or_else(|| Error::Format(format!("a sprite is not {N} numbers")))?;
            let mut out = [0u32; N];
            for (o, v) in out.iter_mut().zip(a) {
                *o = v.as_u64().and_then(|v| u32::try_from(v).ok()).ok_or_else(|| Error::Format("a sprite field is out of range".into()))?;
            }
            let [atlas, x, y, w, h] = [out[0], out[1], out[2], out[3], out[4]];
            let at = atlases.get(atlas as usize).ok_or_else(|| Error::Format("a sprite names a missing atlas".into()))?;
            if x.checked_add(w).is_none_or(|r| r > at.width) || y.checked_add(h).is_none_or(|b| b > at.height) {
                return Err(Error::Format("a sprite reaches outside its atlas".into()));
            }
            Ok(out)
        })
        .collect()
}

/// Decrypt and check a file. `key` is the 32-byte master key (official or custom, as the header
/// says).
pub fn open(file: &[u8], key: &[u8; 32]) -> Result<Opened> {
    let f = container::OpenedFile::open(file, key)?;
    let m = &f.manifest;
    if m.get("version").and_then(Value::as_u64) != Some(f.header.version as u64) {
        return Err(Error::Format("manifest version differs from the header".into()));
    }
    let (width, height) = (u32_field(m, "width")?, u32_field(m, "height")?);
    let frame_count = u32_field(m, "frameCount")? as usize;
    let fps = m.get("fps").and_then(Value::as_f64).filter(|v| v.is_finite() && *v > 0.0).ok_or_else(|| Error::Format("bad fps".into()))?;
    let entries = m.get("atlases").and_then(Value::as_array).ok_or_else(|| Error::Format("manifest has no atlases".into()))?;
    let mut atlases = Vec::with_capacity(entries.len().min(1024));
    for (i, a) in entries.iter().enumerate() {
        let (offset, length) = (u32_field(a, "offset")? as u64, u32_field(a, "length")? as u64);
        let png = f.block(file, i as u32 + 1, offset, length)?;
        let bm = atlas::decode_png(&png)?;
        if (bm.width, bm.height) != (u32_field(a, "width")?, u32_field(a, "height")?) {
            return Err(Error::Format(format!("atlas {i} is not the size the manifest says")));
        }
        atlases.push(bm);
    }
    let content = if f.header.version == 1 {
        let sprites = sprite_list::<7>(m, &atlases)?;
        let frames: Vec<i64> = m
            .get("frames")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Format("manifest has no frames".into()))?
            .iter()
            .map(|v| v.as_i64().unwrap_or(i64::MIN))
            .collect();
        if frames.len() != frame_count || frames.iter().any(|&s| s != -1 && (s < 0 || s as usize >= sprites.len())) {
            return Err(Error::Format("frames list is malformed".into()));
        }
        Content::V1 { sprites, frames }
    } else {
        let sprites = sprite_list::<5>(m, &atlases)?;
        let t = m.get("timeline").ok_or_else(|| Error::Format("manifest has no timeline".into()))?;
        let bytes = f.block(file, atlases.len() as u32 + 1, u32_field(t, "offset")? as u64, u32_field(t, "length")? as u64)?;
        let frames = container::decode_timeline(&bytes, frame_count, u32_field(t, "draws")? as usize)?;
        if frames.iter().flatten().any(|d| d.sprite as usize >= sprites.len()) {
            return Err(Error::Format("a draw names a missing sprite".into()));
        }
        Content::V2 { sprites, frames }
    };
    Ok(Opened {
        header: f.header,
        manifest: f.manifest.clone(),
        width,
        height,
        fps,
        opaque: m.get("opaque").and_then(Value::as_bool).unwrap_or(false),
        atlases,
        content,
    })
}

#[cfg(test)]
mod tests;
