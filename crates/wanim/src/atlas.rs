//! Atlas packing and PNG coding.
//!
//! Rectangles are packed on shelves in the order they are given (the order of first use), so
//! atlases come out in playback order, as players that stream them expect. Every rectangle keeps
//! a 2-pixel transparent margin to the atlas edge and to its neighbours. Atlases are cropped to
//! what they hold, so they are not powers of two.

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
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ATLAS);
    limits.max_image_height = Some(MAX_ATLAS);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| Error::Format(format!("atlas is not a PNG: {e}")))?.to_rgba8();
    Ok(Bitmap { width: img.width(), height: img.height(), rgba: img.into_raw() })
}
