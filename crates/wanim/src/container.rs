//! The `.wanim` container: header, key derivation, encrypted blocks, manifest, the version 2
//! timeline and the version 3 scene (see the crate docs for the layout).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use serde_json::{Value, json};
use sha2::Sha256;

use crate::{Error, Result};

pub const MAGIC: [u8; 8] = [0x89, 0x57, 0x41, 0x4E, 0x49, 0x4D, 0x0D, 0x0A];
pub const HEADER_LEN: usize = 40;
pub const TAG_LEN: usize = 16;
pub const FLAG_CUSTOM_KEY: u16 = 1;
pub const HKDF_INFO: &[u8] = b"wanim/v1/aes-256-gcm";
pub const DRAW_BYTES: usize = 32;
/// Largest manifest a reader accepts (a manifest lists every sprite, so it can be large, but not
/// unbounded).
const MAX_MANIFEST: usize = 64 << 20;
/// The version whose manifest and scene are deflated before they are encrypted.
pub const ANIMATIONS_VERSION: u16 = 3;

/// Raw DEFLATE (RFC 1951, no zlib or gzip header), as version 3 stores its manifest and scene.
pub(crate) fn deflate(bytes: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(bytes, 9)
}

/// Inflate a raw DEFLATE stream of at most `limit` bytes.
pub(crate) fn inflate(bytes: &[u8], limit: usize) -> Result<Vec<u8>> {
    miniz_oxide::inflate::decompress_to_vec_with_limit(bytes, limit).map_err(|_| Error::Format("a block could not be inflated".into()))
}

/// A 32-byte master key and whether it is the official key (flag bit 0 clear) or a custom one.
#[derive(Clone, PartialEq, Eq)]
pub struct Key {
    bytes: [u8; 32],
    custom: bool,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key itself.
        f.debug_struct("Key").field("custom", &self.custom).finish_non_exhaustive()
    }
}

impl Key {
    /// The official key, which every official player has built in. It is never part of this
    /// repository: the caller reads it from the user's settings.
    pub fn official(hex: &str) -> Result<Key> {
        Ok(Key { bytes: parse_hex_key(hex)?, custom: false })
    }

    /// A custom key: the file says so in its flags, and the app that plays it supplies the key.
    pub fn custom(hex: &str) -> Result<Key> {
        Ok(Key { bytes: parse_hex_key(hex)?, custom: true })
    }

    pub fn is_custom(&self) -> bool {
        self.custom
    }
}

/// 64 hexadecimal characters (whitespace around them ignored) → 32 bytes.
pub fn parse_hex_key(hex: &str) -> Result<[u8; 32]> {
    let hex = hex.trim();
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return Err(Error::Key(format!("a key is 64 hexadecimal characters, this one has {}", bytes.len())));
    }
    let mut out = [0u8; 32];
    for (o, &[h, l]) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
        let digit = |c: u8| -> Result<u8> {
            match c {
                b'0'..=b'9' => Ok(c - b'0'),
                b'a'..=b'f' => Ok(c - b'a' + 10),
                b'A'..=b'F' => Ok(c - b'A' + 10),
                _ => Err(Error::Key("a key may contain only hexadecimal characters".into())),
            }
        };
        *o = digit(h)? << 4 | digit(l)?;
    }
    Ok(out)
}

/// The per-file salt and nonce prefix (random for every file written).
#[derive(Clone, Copy, Debug)]
pub struct Nonces {
    pub salt: [u8; 16],
    pub nonce_prefix: [u8; 8],
}

impl Nonces {
    /// Fresh random values from the operating system (the browser on the web).
    pub fn random() -> Result<Nonces> {
        let mut b = [0u8; 24];
        getrandom::getrandom(&mut b).map_err(|e| Error::Crypto(format!("no random numbers available: {e}")))?;
        let mut salt = [0u8; 16];
        let mut nonce_prefix = [0u8; 8];
        salt.copy_from_slice(b.get(..16).unwrap_or(&[0; 16]));
        nonce_prefix.copy_from_slice(b.get(16..24).unwrap_or(&[0; 8]));
        Ok(Nonces { salt, nonce_prefix })
    }
}

struct BlockCipher {
    cipher: Aes256Gcm,
    header: [u8; HEADER_LEN],
    nonce_prefix: [u8; 8],
}

impl BlockCipher {
    fn new(key: &[u8; 32], header: [u8; HEADER_LEN]) -> Result<BlockCipher> {
        let salt = header.get(16..32).ok_or_else(|| Error::Format("short header".into()))?;
        let mut file_key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(salt), key).expand(HKDF_INFO, &mut file_key).map_err(|e| Error::Crypto(e.to_string()))?;
        let cipher = Aes256Gcm::new_from_slice(&file_key).map_err(|e| Error::Crypto(e.to_string()))?;
        let mut nonce_prefix = [0u8; 8];
        nonce_prefix.copy_from_slice(header.get(32..40).ok_or_else(|| Error::Format("short header".into()))?);
        Ok(BlockCipher { cipher, header, nonce_prefix })
    }

    fn nonce_aad(&self, index: u32) -> ([u8; 12], [u8; HEADER_LEN + 4]) {
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(&self.nonce_prefix);
        nonce[8..].copy_from_slice(&index.to_le_bytes());
        let mut aad = [0u8; HEADER_LEN + 4];
        aad[..HEADER_LEN].copy_from_slice(&self.header);
        aad[HEADER_LEN..].copy_from_slice(&index.to_le_bytes());
        (nonce, aad)
    }

    fn seal(&self, index: u32, plain: &[u8]) -> Result<Vec<u8>> {
        let (nonce, aad) = self.nonce_aad(index);
        self.cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad: &aad }).map_err(|_| Error::Crypto("encryption failed".into()))
    }

    fn open(&self, index: u32, sealed: &[u8]) -> Result<Vec<u8>> {
        let (nonce, aad) = self.nonce_aad(index);
        self.cipher
            .decrypt(Nonce::from_slice(&nonce), Payload { msg: sealed, aad: &aad })
            .map_err(|_| Error::Crypto(format!("block {index} failed its tag check: wrong key, or the file was modified")))
    }
}

fn header(version: u16, custom: bool, manifest_len: u32, n: &Nonces) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(&MAGIC);
    h[8..10].copy_from_slice(&version.to_le_bytes());
    h[10..12].copy_from_slice(&(if custom { FLAG_CUSTOM_KEY } else { 0 }).to_le_bytes());
    h[12..16].copy_from_slice(&manifest_len.to_le_bytes());
    h[16..32].copy_from_slice(&n.salt);
    h[32..40].copy_from_slice(&n.nonce_prefix);
    h
}

/// Encrypt and lay out a file: `manifest` gets each data block's `offset`/`length` filled in by
/// `place` (called with the sealed lengths, in block order) before it is sealed as block 0.
/// `blocks` are the plaintext data blocks (block 1, 2, …).
pub(crate) fn write_file(version: u16, key: &Key, nonces: &Nonces, blocks: &[Vec<u8>], place: impl FnOnce(&[(u64, u64)]) -> Value) -> Result<(Vec<u8>, Value)> {
    // The sealed size is known up front (plaintext + tag), so the manifest can name every offset
    // before anything is encrypted; its own sealed length goes into the header, which is part of
    // every block's associated data.
    let mut spans = Vec::with_capacity(blocks.len());
    let mut offset = 0u64;
    for b in blocks {
        let len = b.len() as u64 + TAG_LEN as u64;
        spans.push((offset, len));
        offset = offset.checked_add(len).ok_or_else(|| Error::Format("file too large".into()))?;
    }
    let manifest = place(&spans);
    let mut manifest_bytes = serde_json::to_vec(&manifest).map_err(|e| Error::Format(e.to_string()))?;
    if version == ANIMATIONS_VERSION {
        manifest_bytes = deflate(&manifest_bytes);
    }
    let manifest_len = u32::try_from(manifest_bytes.len() + TAG_LEN).map_err(|_| Error::Format("manifest too large".into()))?;
    let head = header(version, key.custom, manifest_len, nonces);
    let bc = BlockCipher::new(&key.bytes, head)?;
    let mut out = Vec::with_capacity(HEADER_LEN + manifest_len as usize + offset as usize);
    out.extend_from_slice(&head);
    out.extend_from_slice(&bc.seal(0, &manifest_bytes)?);
    for (i, b) in blocks.iter().enumerate() {
        let index = u32::try_from(i + 1).map_err(|_| Error::Format("too many blocks".into()))?;
        out.extend_from_slice(&bc.seal(index, b)?);
    }
    Ok((out, manifest))
}

/// The header fields a reader can see without a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u16,
    pub custom_key: bool,
    pub manifest_len: u32,
}

pub fn read_header(file: &[u8]) -> Result<Header> {
    let h = file.get(..HEADER_LEN).ok_or_else(|| Error::Format("not a .wanim file: too short".into()))?;
    if h.get(..8) != Some(&MAGIC[..]) {
        return Err(Error::Format("not a .wanim file: wrong magic".into()));
    }
    let u16_at = |o: usize| h.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
    let version = u16_at(8);
    if !(1..=ANIMATIONS_VERSION).contains(&version) {
        return Err(Error::Format(format!("unsupported .wanim version {version}")));
    }
    let flags = u16_at(10);
    if flags & !FLAG_CUSTOM_KEY != 0 {
        return Err(Error::Format(format!("unknown .wanim flags {flags:#x}")));
    }
    let manifest_len = h.get(12..16).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
    Ok(Header { version, custom_key: flags & FLAG_CUSTOM_KEY != 0, manifest_len })
}

/// A decrypted file: its manifest and every data block (atlases, then the timeline or scene).
pub(crate) struct OpenedFile {
    pub header: Header,
    pub manifest: Value,
    data_start: usize,
    bc: BlockCipher,
}

impl OpenedFile {
    pub fn open(file: &[u8], key: &[u8; 32]) -> Result<OpenedFile> {
        let header = read_header(file)?;
        let mlen = header.manifest_len as usize;
        if !(TAG_LEN..=MAX_MANIFEST).contains(&mlen) {
            return Err(Error::Format("manifest length out of range".into()));
        }
        let mut head = [0u8; HEADER_LEN];
        head.copy_from_slice(file.get(..HEADER_LEN).ok_or_else(|| Error::Format("short header".into()))?);
        let bc = BlockCipher::new(key, head)?;
        let sealed = file.get(HEADER_LEN..HEADER_LEN + mlen).ok_or_else(|| Error::Format("manifest runs past the end of the file".into()))?;
        let mut plain = bc.open(0, sealed)?;
        if header.version == ANIMATIONS_VERSION {
            plain = inflate(&plain, MAX_MANIFEST)?;
        }
        let manifest: Value = serde_json::from_slice(&plain).map_err(|e| Error::Format(format!("manifest is not JSON: {e}")))?;
        Ok(OpenedFile { header, manifest, data_start: HEADER_LEN + mlen, bc })
    }

    /// Decrypt data block `index` (1 = atlas 0) at `offset`/`length` in the data section.
    pub fn block(&self, file: &[u8], index: u32, offset: u64, length: u64) -> Result<Vec<u8>> {
        let start =
            usize::try_from(offset).ok().and_then(|o| o.checked_add(self.data_start)).ok_or_else(|| Error::Format("block offset out of range".into()))?;
        let end = usize::try_from(length).ok().and_then(|l| start.checked_add(l)).ok_or_else(|| Error::Format("block length out of range".into()))?;
        let sealed = file.get(start..end).ok_or_else(|| Error::Format(format!("block {index} runs past the end of the file")))?;
        self.bc.open(index, sealed)
    }
}

/// The manifest's `atlases` entry for a placed block.
pub(crate) fn atlas_entry(span: (u64, u64), width: u32, height: u32, format: crate::atlas::AtlasFormat) -> Value {
    json!({"offset": span.0, "length": span.1, "width": width, "height": height, "format": format.name()})
}

/// Encode a version 2 timeline: `frameStart[frameCount + 1]`, then 32 bytes per draw.
pub(crate) fn encode_timeline(frames: &[Vec<crate::Draw>]) -> Result<Vec<u8>> {
    let draws: usize = frames.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity((frames.len() + 1) * 4 + draws * DRAW_BYTES);
    let mut start = 0u32;
    for f in frames {
        out.extend_from_slice(&start.to_le_bytes());
        start = start
            .checked_add(u32::try_from(f.len()).map_err(|_| Error::Format("too many draws".into()))?)
            .ok_or_else(|| Error::Format("too many draws".into()))?;
    }
    out.extend_from_slice(&start.to_le_bytes());
    for d in frames.iter().flatten() {
        out.extend_from_slice(&d.sprite.to_le_bytes());
        for v in d.matrix {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&d.alpha.to_le_bytes());
    }
    Ok(out)
}

/// Decode a version 2 timeline, checking its size and that `frameStart` never decreases.
pub(crate) fn decode_timeline(bytes: &[u8], frame_count: usize, draws: usize) -> Result<Vec<Vec<crate::Draw>>> {
    let bad = || Error::Format("timeline has the wrong size".into());
    let expect =
        frame_count.checked_add(1).and_then(|n| n.checked_mul(4)).and_then(|a| draws.checked_mul(DRAW_BYTES).and_then(|b| a.checked_add(b))).ok_or_else(bad)?;
    if bytes.len() != expect {
        return Err(bad());
    }
    let u32_at = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let f32_at = |o: usize| bytes.get(o..o + 4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let starts: Vec<usize> = (0..=frame_count).map(|i| u32_at(i * 4).map(|v| v as usize)).collect::<Option<_>>().ok_or_else(bad)?;
    if starts.windows(2).any(|w| w[1] < w[0]) || starts.last() != Some(&draws) || starts.first() != Some(&0) {
        return Err(Error::Format("timeline frame starts are out of order".into()));
    }
    let base = (frame_count + 1) * 4;
    let draw_at = |i: usize| -> Option<crate::Draw> {
        let o = base + i * DRAW_BYTES;
        let mut m = [0f32; 6];
        for (k, v) in m.iter_mut().enumerate() {
            *v = f32_at(o + 4 + k * 4)?;
        }
        Some(crate::Draw { sprite: u32_at(o)?, matrix: m, alpha: f32_at(o + 28)? })
    };
    starts.windows(2).map(|w| (w[0]..w[1]).map(draw_at).collect::<Option<Vec<_>>>().ok_or_else(bad)).collect()
}

/// The decoded scene of a version 3 file: every different draw once, every different frame once
/// (a list of draws, back to front), and the list each frame shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub draws: Vec<crate::Draw>,
    pub lists: Vec<Vec<u32>>,
    pub frames: Vec<u32>,
}

/// Encode a version 3 scene: one column per draw field (`u32 sprite[D]`, then `a`, `b`, `c`, `d`,
/// `tx`, `ty` and `alpha` as `f32[D]`), `u32 listStart[L + 1]`, `u32 items[I]`, `u32 frames[F]`.
/// Returns the bytes and `I`.
pub(crate) fn encode_scene(scene: &Scene) -> Result<(Vec<u8>, u32)> {
    let many = || Error::TooLarge("too many draws".into());
    let items: usize = scene.lists.iter().map(Vec::len).sum();
    let item_count = u32::try_from(items).map_err(|_| many())?;
    let mut out = Vec::with_capacity(scene.draws.len() * DRAW_BYTES + (scene.lists.len() + 1 + items + scene.frames.len()) * 4);
    for d in &scene.draws {
        out.extend_from_slice(&d.sprite.to_le_bytes());
    }
    for column in 0..6 {
        for d in &scene.draws {
            out.extend_from_slice(&d.matrix.get(column).copied().unwrap_or(0.0).to_le_bytes());
        }
    }
    for d in &scene.draws {
        out.extend_from_slice(&d.alpha.to_le_bytes());
    }
    let mut start = 0u32;
    for l in &scene.lists {
        out.extend_from_slice(&start.to_le_bytes());
        start = u32::try_from(l.len()).ok().and_then(|n| start.checked_add(n)).ok_or_else(many)?;
    }
    out.extend_from_slice(&start.to_le_bytes());
    for item in scene.lists.iter().flatten() {
        out.extend_from_slice(&item.to_le_bytes());
    }
    for f in &scene.frames {
        out.extend_from_slice(&f.to_le_bytes());
    }
    Ok((out, item_count))
}

/// The inflated size of a scene with `draws` draws, `lists` lists, `items` items and `frames`
/// frames.
pub(crate) fn scene_size(draws: usize, lists: usize, items: usize, frames: usize) -> Option<usize> {
    let words = lists.checked_add(1)?.checked_add(items)?.checked_add(frames)?;
    draws.checked_mul(DRAW_BYTES)?.checked_add(words.checked_mul(4)?)
}

/// Decode an inflated version 3 scene, checking its size, that every sprite, draw and list index
/// exists (`sprites` is the number of sprites), that `listStart` starts at 0, never decreases and
/// ends at `items`.
pub(crate) fn decode_scene(bytes: &[u8], draws: usize, lists: usize, items: usize, frames: usize, sprites: usize) -> Result<Scene> {
    let bad = || Error::Format("scene has the wrong size".into());
    if scene_size(draws, lists, items, frames) != Some(bytes.len()) {
        return Err(bad());
    }
    let u32_at = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let f32_at = |o: usize| bytes.get(o..o + 4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let column = |c: usize, i: usize| (c * draws + i) * 4;
    let mut out = Scene::default();
    for i in 0..draws {
        let sprite = u32_at(column(0, i)).ok_or_else(bad)?;
        if sprite as usize >= sprites {
            return Err(Error::Format("a draw names a missing sprite".into()));
        }
        let mut matrix = [0f32; 6];
        for (k, v) in matrix.iter_mut().enumerate() {
            *v = f32_at(column(k + 1, i)).ok_or_else(bad)?;
        }
        out.draws.push(crate::Draw { sprite, matrix, alpha: f32_at(column(7, i)).ok_or_else(bad)? });
    }
    let lists_at = draws * DRAW_BYTES;
    let items_at = lists_at + (lists + 1) * 4;
    let frames_at = items_at + items * 4;
    let starts: Vec<usize> = (0..=lists).map(|i| u32_at(lists_at + i * 4).map(|v| v as usize)).collect::<Option<_>>().ok_or_else(bad)?;
    if starts.first() != Some(&0) || starts.windows(2).any(|w| w[1] < w[0]) || starts.last() != Some(&items) {
        return Err(Error::Format("scene list starts are out of order".into()));
    }
    for w in starts.windows(2) {
        let list: Vec<u32> = (w[0]..w[1]).map(|i| u32_at(items_at + i * 4)).collect::<Option<_>>().ok_or_else(bad)?;
        if list.iter().any(|&d| d as usize >= draws) {
            return Err(Error::Format("a list names a missing draw".into()));
        }
        out.lists.push(list);
    }
    out.frames = (0..frames).map(|i| u32_at(frames_at + i * 4)).collect::<Option<_>>().ok_or_else(bad)?;
    if out.frames.iter().any(|&l| l as usize >= lists) {
        return Err(Error::Format("a frame names a missing list".into()));
    }
    Ok(out)
}

/// The decrypted block 0 as stored (version 3: still deflated), for tests of the layout.
#[cfg(test)]
pub(crate) fn stored_manifest(file: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    let header = read_header(file)?;
    let mut head = [0u8; HEADER_LEN];
    head.copy_from_slice(file.get(..HEADER_LEN).ok_or_else(|| Error::Format("short header".into()))?);
    let sealed = file.get(HEADER_LEN..HEADER_LEN + header.manifest_len as usize).ok_or_else(|| Error::Format("short manifest".into()))?;
    BlockCipher::new(key, head)?.open(0, sealed)
}
