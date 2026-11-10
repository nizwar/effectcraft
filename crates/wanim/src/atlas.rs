//! Atlas packing and PNG coding.
//!
//! Rectangles are packed on shelves in the order they are given (the order of first use), so
//! atlases come out in playback order, as players that stream them expect. Every rectangle keeps
//! a 2-pixel transparent margin to the atlas edge and to its neighbours. Atlases are cropped to
//! what they hold, so they are not powers of two.
//!
//! Version 3 packs tighter ([`pack_sprites`]): sprites touch (each carries its own 1-pixel
//! transparent border), a page's sprites are sorted by height, and the page takes the width that
//! needs the fewest pixels. Its atlases may be WebP ([`AtlasFormat`]).

use crate::{Bitmap, Error, Result};

/// Space between rectangles and around the atlas edge.
pub const GUTTER: u32 = 2;
/// The largest atlas side the format allows.
pub const MAX_ATLAS: u32 = 4096;

/// Where a rectangle landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub page: u32,
    pub x: u32,
    pub y: u32,
}

/// One atlas page's final size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub width: u32,
    pub height: u32,
}

struct Shelf {
    y: u32,
    height: u32,
    x: u32,
}

/// Pack `sizes` (`(w, h)`) into pages of at most `page_size` square.
pub fn shelf_pack(sizes: &[(u32, u32)], page_size: u32) -> Result<(Vec<Page>, Vec<Place>)> {
    let page_size = page_size.clamp(64, MAX_ATLAS);
    let room = page_size - 2 * GUTTER;
    let mut pages: Vec<Page> = vec![];
    let mut places = Vec::with_capacity(sizes.len());
    let mut shelves: Vec<Shelf> = vec![];
    for &(w, h) in sizes {
        if w == 0 || h == 0 {
            return Err(Error::Format("a sprite has no pixels".into()));
        }
        if w > room || h > room {
            return Err(Error::TooLarge(format!(
                "a {w}×{h} sprite does not fit a {page_size}-pixel atlas: export at a smaller scale, or raise the atlas size (at most {MAX_ATLAS})"
            )));
        }
        if pages.is_empty() {
            pages.push(Page { width: 0, height: 0 });
        }
        let place = match fit(&mut shelves, w, h, page_size) {
            Some(p) => p,
            None => {
                // The page is full: start the next one.
                shelves.clear();
                pages.push(Page { width: 0, height: 0 });
                fit(&mut shelves, w, h, page_size).ok_or_else(|| Error::TooLarge("sprite does not fit an empty atlas".into()))?
            }
        };
        let page_index = pages.len() - 1;
        let Some(page) = pages.last_mut() else { return Err(Error::Format("no atlas page".into())) };
        page.width = page.width.max(place.0 + w + GUTTER);
        page.height = page.height.max(place.1 + h + GUTTER);
        places.push(Place { page: page_index as u32, x: place.0, y: place.1 });
    }
    Ok((pages, places))
}

/// Put a `w`×`h` rectangle on an existing shelf or a new one; `None` when the page is full.
fn fit(shelves: &mut Vec<Shelf>, w: u32, h: u32, page_size: u32) -> Option<(u32, u32)> {
    let limit = page_size - GUTTER;
    let last = shelves.len().saturating_sub(1);
    for (i, s) in shelves.iter_mut().enumerate() {
        // The last (open) shelf may grow taller while the page has room below it.
        let fits_height = h <= s.height || (i == last && s.y + h <= limit);
        if fits_height && s.x + w <= limit {
            s.height = s.height.max(h);
            let at = (s.x, s.y);
            s.x += w + GUTTER;
            return Some(at);
        }
    }
    let y = shelves.last().map_or(GUTTER, |s| s.y + s.height + GUTTER);
    if y + h > limit {
        return None;
    }
    shelves.push(Shelf { y, height: h, x: GUTTER + w + GUTTER });
    Some((GUTTER, y))
}

/// Copy `bitmaps` into their pages (straight-alpha RGBA, transparent elsewhere) and encode each
/// page as a PNG.
pub fn render_pages(pages: &[Page], places: &[Place], bitmaps: &[&Bitmap]) -> Result<Vec<Vec<u8>>> {
    let mut pixels: Vec<Vec<u8>> = pages.iter().map(|p| vec![0u8; p.width as usize * p.height as usize * 4]).collect();
    for (place, bm) in places.iter().zip(bitmaps) {
        let page = pages.get(place.page as usize).ok_or_else(|| Error::Format("sprite on a missing page".into()))?;
        let dst = pixels.get_mut(place.page as usize).ok_or_else(|| Error::Format("sprite on a missing page".into()))?;
        let row = bm.width as usize * 4;
        for y in 0..bm.height as usize {
            let src = bm.rgba.get(y * row..(y + 1) * row).ok_or_else(|| Error::Format("bitmap is shorter than its size".into()))?;
            let at = ((place.y as usize + y) * page.width as usize + place.x as usize) * 4;
            dst.get_mut(at..at + row).ok_or_else(|| Error::Format("sprite runs past its atlas".into()))?.copy_from_slice(src);
        }
    }
    pages.iter().zip(pixels).map(|(p, px)| encode_png(p.width, p.height, &px)).collect()
}

/// Shelves of one page: `order` (indices into `sizes`, highest first) placed first-fit on rows of
/// at most `page_width`. Returns the places and the page's `(width, height)`.
fn shelves(sizes: &[(u32, u32)], order: &[usize], page_width: u32) -> (Vec<(usize, u32, u32)>, u32, u32) {
    // (y, height, x of the next sprite)
    let mut rows: Vec<(u32, u32, u32)> = vec![];
    let mut places = Vec::with_capacity(order.len());
    let mut width = 0u32;
    for &index in order {
        let Some(&(w, h)) = sizes.get(index) else { continue };
        let row = match rows.iter().position(|r| r.2.saturating_add(w) <= page_width) {
            Some(i) => i,
            None => {
                let y = rows.last().map_or(0, |r| r.0.saturating_add(r.1));
                rows.push((y, h, 0));
                rows.len() - 1
            }
        };
        let Some(r) = rows.get_mut(row) else { continue };
        places.push((index, r.2, r.0));
        r.2 = r.2.saturating_add(w);
        width = width.max(r.2);
    }
    (places, width, rows.last().map_or(0, |r| r.0.saturating_add(r.1)))
}

/// Pack `sizes` (`(w, h)`, borders included) into pages of at most `page_size` square, without
/// space between them (version 3). Pages are filled in the given order, so sprites that are first
/// used together share a page; on a page they are sorted by height and put on shelves, and the
/// page is as wide as needs the fewest pixels.
pub fn pack_sprites(sizes: &[(u32, u32)], page_size: u32) -> Result<(Vec<Page>, Vec<Place>)> {
    let page_size = page_size.clamp(1, MAX_ATLAS);
    for &(w, h) in sizes {
        if w == 0 || h == 0 {
            return Err(Error::Format("a sprite has no pixels".into()));
        }
        if w > page_size || h > page_size {
            return Err(Error::TooLarge(format!(
                "a {w}×{h} sprite does not fit a {page_size}-pixel atlas: export at a smaller scale, or raise the atlas size (at most {MAX_ATLAS})"
            )));
        }
    }
    let by_height = |from: usize, to: usize| -> Vec<usize> {
        let mut order: Vec<usize> = (from..to).collect();
        let size = |i: usize| sizes.get(i).copied().unwrap_or((0, 0));
        order.sort_by(|&a, &b| size(b).1.cmp(&size(a).1).then(size(b).0.cmp(&size(a).0)).then(a.cmp(&b)));
        order
    };
    let fits = |from: usize, to: usize| shelves(sizes, &by_height(from, to), page_size).2 <= page_size;
    let mut pages = vec![];
    let mut places = vec![Place { page: 0, x: 0, y: 0 }; sizes.len()];
    let mut from = 0;
    while from < sizes.len() {
        // The longest run of sprites that fits on one page.
        let mut to = sizes.len();
        if !fits(from, to) {
            let (mut low, mut high) = (from + 1, to - 1);
            while low < high {
                let middle = (low + high).div_ceil(2);
                if fits(from, middle) {
                    low = middle;
                } else {
                    high = middle - 1;
                }
            }
            to = low;
        }
        let order = by_height(from, to);
        let widest = order.iter().filter_map(|&i| sizes.get(i)).map(|s| s.0).max().unwrap_or(1);
        let area: f64 = order.iter().filter_map(|&i| sizes.get(i)).map(|s| s.0 as f64 * s.1 as f64).sum();
        // Page widths to try: from a square of the sprites' area up to four times as wide.
        let mut candidates = vec![page_size];
        let mut factor = 1.0f64;
        while factor <= 4.0 {
            let wide = (area.sqrt() * factor).ceil().min(MAX_ATLAS as f64) as u32;
            candidates.push(page_size.min(widest.max(wide)));
            factor += 0.05;
        }
        candidates.sort_unstable();
        candidates.dedup();
        let mut best: Option<(Vec<(usize, u32, u32)>, u32, u32)> = None;
        for page_width in candidates {
            let result = shelves(sizes, &order, page_width);
            let smaller = best.as_ref().is_none_or(|b| (result.1 as u64 * result.2 as u64) < b.1 as u64 * b.2 as u64);
            if result.2 <= page_size && smaller {
                best = Some(result);
            }
        }
        let Some((placed, width, height)) = best else { return Err(Error::TooLarge("the sprites could not be placed on an atlas".into())) };
        let page = u32::try_from(pages.len()).map_err(|_| Error::TooLarge("too many atlases".into()))?;
        for (index, x, y) in placed {
            if let Some(p) = places.get_mut(index) {
                *p = Place { page, x, y };
            }
        }
        pages.push(Page { width, height });
        from = to;
    }
    Ok((pages, places))
}

/// How an atlas is stored. Version 1 and 2 files hold PNG only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AtlasFormat {
    #[default]
    Png,
    /// Lossless WebP (version 3). This encoder's WebP is smaller than its PNG for most pictures and
    /// larger for some.
    Webp,
    /// Each atlas as PNG or lossless WebP, whichever is fewer bytes (version 3).
    Smallest,
}

impl AtlasFormat {
    /// The manifest's `atlases[].format`.
    pub fn name(self) -> &'static str {
        match self {
            AtlasFormat::Png => "png",
            AtlasFormat::Webp => "webp",
            AtlasFormat::Smallest => "smallest",
        }
    }

    pub fn from_name(name: &str) -> Option<AtlasFormat> {
        match name {
            "png" => Some(AtlasFormat::Png),
            "webp" => Some(AtlasFormat::Webp),
            "smallest" => Some(AtlasFormat::Smallest),
            _ => None,
        }
    }
}

/// Encode straight-alpha RGBA as an atlas in `format`, and say which format it is in:
/// [`AtlasFormat::Smallest`] gives PNG or WebP.
pub fn encode(format: AtlasFormat, width: u32, height: u32, rgba: &[u8]) -> Result<(AtlasFormat, Vec<u8>)> {
    let webp = || -> Result<Vec<u8>> {
        let mut out = vec![];
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .encode(rgba, width, height, image::ExtendedColorType::Rgba8)
            .map_err(|e| Error::Format(format!("WebP encoding failed: {e}")))?;
        Ok(out)
    };
    match format {
        AtlasFormat::Png => Ok((AtlasFormat::Png, encode_png(width, height, rgba)?)),
        AtlasFormat::Webp => Ok((AtlasFormat::Webp, webp()?)),
        AtlasFormat::Smallest => {
            let (png, webp) = (encode_png(width, height, rgba)?, webp()?);
            Ok(if webp.len() < png.len() { (AtlasFormat::Webp, webp) } else { (AtlasFormat::Png, png) })
        }
    }
}

/// Decode an atlas in `format` to straight-alpha RGBA (larger than [`MAX_ATLAS`] is refused).
pub fn decode(format: AtlasFormat, bytes: &[u8]) -> Result<Bitmap> {
    match format {
        AtlasFormat::Png => decode_png(bytes),
        AtlasFormat::Webp => decode_as(bytes, image::ImageFormat::WebP, "WebP"),
        AtlasFormat::Smallest => Err(Error::Format("an atlas is stored as PNG or WebP".into())),
    }
}

pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = vec![];
    image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| Error::Format(format!("PNG encoding failed: {e}")))?;
    Ok(out)
}

/// Decode an atlas PNG to straight-alpha RGBA (any 8-bit PNG; larger than [`MAX_ATLAS`] is refused).
pub fn decode_png(bytes: &[u8]) -> Result<Bitmap> {
    decode_as(bytes, image::ImageFormat::Png, "PNG")
}

fn decode_as(bytes: &[u8], format: image::ImageFormat, name: &str) -> Result<Bitmap> {
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ATLAS);
    limits.max_image_height = Some(MAX_ATLAS);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| Error::Format(format!("atlas is not a {name}: {e}")))?.to_rgba8();
    Ok(Bitmap { width: img.width(), height: img.height(), rgba: img.into_raw() })
}
