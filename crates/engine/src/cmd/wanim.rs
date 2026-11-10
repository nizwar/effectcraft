//! File ▸ Export ▸ WANIM… (`effectcraft-wanim`): the comp as an encrypted `.wanim` sprite
//! animation, version 3 (animations played by key, frames as layers) by default.
//!
//! A version 3 file holds one animation, `main`, or the ones named by `animations`: a list of
//! keys with frame ranges, or `"markers"` for the comp markers that have a duration
//! ([`marker_ranges`]).
//!
//! Files use the official key unless the command is given a custom `key`. The official key is
//! never part of EffectCraft's source: it is read from [`OFFICIAL_KEY_FILE`] in the settings
//! folder (`file.setWanimKey` writes it), or else built in when the binary was compiled with
//! `EFFECTCRAFT_WANIM_OFFICIAL_KEY` set ([`BUILT_IN_KEY`]).

use effectcraft_project::Comp;
use effectcraft_render::{Backend, LayerCache, RenderOpts, Renderer};
use effectcraft_wanim::plan::{Range, Settings, Tolerance};
use effectcraft_wanim::{AnimationsOptions, AtlasFormat, Key, Nonces, WriteOptions};
use serde_json::{Value, json};

use super::{CommandSpec, always, b_p, bad, comp_id, f_p, has_comp, str_p};
use crate::{EngineError, Result, Session, cmd};

/// The settings-folder entry that holds the official key (64 hexadecimal characters).
pub const OFFICIAL_KEY_FILE: &str = "wanim_official_key.txt";

fn err(e: effectcraft_wanim::Error) -> EngineError {
    EngineError::Other(e.to_string())
}

/// The official key built into this binary: `EFFECTCRAFT_WANIM_OFFICIAL_KEY` at compile time.
/// Builds without it (every build from the repository as it is) have none.
pub const BUILT_IN_KEY: Option<&str> = option_env!("EFFECTCRAFT_WANIM_OFFICIAL_KEY");

/// The custom `key` parameter, else the official key from the settings folder, else the one built
/// in (`built_in`).
fn key(s: &Session, p: &Value, built_in: Option<&str>) -> Result<Key> {
    if let Some(hex) = str_p(p, "key") {
        return Key::custom(hex).map_err(err);
    }
    let saved = match s.config.as_ref() {
        Some(cfg) => cfg.read_bounded(OFFICIAL_KEY_FILE, 1024).map_err(|e| EngineError::Other(format!("cannot read {OFFICIAL_KEY_FILE}: {e}")))?,
        None => None,
    };
    let hex = saved.as_deref().or(built_in.filter(|k| !k.trim().is_empty())).ok_or_else(|| {
        EngineError::Other(format!(
            "no official .wanim key: run file.setWanimKey {{\"key\": \"<64 hex characters>\"}} (it is kept as {OFFICIAL_KEY_FILE} in the settings folder), or pass a custom `key`"
        ))
    })?;
    Key::official(hex).map_err(err)
}

/// A whole number parameter in `lo..=hi`.
fn int_p(p: &Value, k: &str, lo: u32, hi: u32, default: u32) -> Result<u32> {
    let Some(v) = p.get(k).filter(|v| !v.is_null()) else { return Ok(default) };
    v.as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| (lo..=hi).contains(v))
        .ok_or_else(|| bad("file.exportWanim", format!("`{k}` must be a whole number from {lo} to {hi}")))
}

/// The most animations one file is asked to hold.
const MAX_ANIMATIONS: usize = 256;

/// The comp markers that have a duration and a comment, as animations: the comment is the key,
/// or the key followed by `loop` for an animation that repeats (`idle loop`). The animation runs
/// from the marker's frame to the last frame its duration covers.
fn marker_ranges(comp: &Comp) -> Result<Vec<Range>> {
    let rate = comp.frame_rate;
    let last_frame = rate.frame_at(comp.duration) - 1;
    let mut markers: Vec<_> = comp.markers.iter().filter(|m| m.duration > effectcraft_time::Tick::ZERO && !m.comment.trim().is_empty()).collect();
    markers.sort_by_key(|m| m.time);
    let mut out = Vec::with_capacity(markers.len());
    for m in markers {
        let mut words = m.comment.split_whitespace();
        let key = words.next().unwrap_or_default();
        let looped = match (words.next(), words.next()) {
            (None, _) => false,
            (Some(w), None) if w.eq_ignore_ascii_case("loop") => true,
            _ => {
                return Err(bad(
                    "file.exportWanim",
                    format!(
                        "the comp marker at {:.3} s is commented `{}`: with `animations: \"markers\"`, a marker with a duration is commented with its animation key, or the key and `loop`",
                        m.time.seconds(),
                        m.comment.chars().take(80).collect::<String>()
                    ),
                ));
            }
        };
        let first = rate.frame_at(m.time);
        let end = rate.frame_at(effectcraft_time::Tick(m.time.0.saturating_add(m.duration.0)));
        // A marker shorter than a frame is that frame; one that runs past the comp ends with it.
        out.push(Range { key: key.to_string(), looped, first, last: (end - 1).min(last_frame).max(first) });
    }
    if out.is_empty() {
        return Err(bad(
            "file.exportWanim",
            "`animations: \"markers\"` needs comp markers that have a duration and are commented with an animation key (markers.set {new: true, time, duration, comment})",
        ));
    }
    Ok(out)
}

/// The `animations` parameter: absent (one animation, `main`), `"markers"`, or a list of
/// `{key, firstFrame, lastFrame, loop?}`.
fn animation_ranges(comp: &Comp, p: &Value) -> Result<Vec<Range>> {
    let list = match p.get("animations") {
        None | Some(Value::Null) => return Ok(vec![]),
        Some(Value::String(s)) if s == "markers" => return marker_ranges(comp),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(bad("file.exportWanim", "`animations` is a list of {key, firstFrame, lastFrame, loop?}, or \"markers\"")),
    };
    if list.is_empty() || list.len() > MAX_ANIMATIONS {
        return Err(bad("file.exportWanim", format!("`animations` holds 1 to {MAX_ANIMATIONS} animations")));
    }
    list.iter()
        .enumerate()
        .map(|(i, a)| {
            let obj = a.as_object().ok_or_else(|| bad("file.exportWanim", format!("`animations[{i}]` is not {{key, firstFrame, lastFrame, loop?}}")))?;
            if let Some(k) = obj.keys().find(|k| !matches!(k.as_str(), "key" | "firstFrame" | "lastFrame" | "loop")) {
                return Err(bad("file.exportWanim", format!("`animations[{i}]` has an unknown field `{k}`; accepted: key, firstFrame, lastFrame, loop")));
            }
            let key = obj.get("key").and_then(Value::as_str).ok_or_else(|| bad("file.exportWanim", format!("`animations[{i}]` has no `key`")))?;
            let frame = |k: &str| -> Result<i64> {
                obj.get(k)
                    .and_then(Value::as_i64)
                    .ok_or_else(|| bad("file.exportWanim", format!("`animations[{i}].{k}` (animation `{key}`) must be a whole frame number")))
            };
            let looped = match obj.get("loop") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(bad("file.exportWanim", format!("`animations[{i}].loop` (animation `{key}`) must be true or false"))),
            };
            Ok(Range { key: key.to_string(), looped, first: frame("firstFrame")?, last: frame("lastFrame")? })
        })
        .collect()
}

fn export_wanim(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("file.exportWanim", "missing `path`"))?.to_string();
    s.refuse_export(&path)?;
    let cid = comp_id(s, p)?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let version = int_p(p, "version", 1, 3, 3)? as u16;
    let every_nth = int_p(p, "everyNthFrame", 1, 1000, 1)?;
    let max_atlas = effectcraft_wanim::atlas::MAX_ATLAS;
    // Version 3 picks its own atlas size unless one is given.
    let atlas_size = p.get("atlasSize").filter(|v| !v.is_null()).map(|_| int_p(p, "atlasSize", 256, max_atlas, max_atlas)).transpose()?;
    let scale = f_p(p, "scale").unwrap_or(1.0);
    if !(scale.is_finite() && scale > 0.0 && scale <= 4.0) {
        return Err(bad("file.exportWanim", "`scale` must be above 0 and at most 4"));
    }
    let atlas_format = match p.get("atlasFormat").filter(|v| !v.is_null()) {
        None => AtlasFormat::Png,
        Some(v) => v.as_str().and_then(AtlasFormat::from_name).ok_or_else(|| bad("file.exportWanim", "`atlasFormat` is \"png\" or \"webp\""))?,
    };
    let work_area = b_p(p, "workArea").unwrap_or(false);
    if version < 3 && p.get("animations").is_some_and(|v| !v.is_null()) {
        return Err(bad("file.exportWanim", format!("`animations` needs version 3: a version {version} file holds one animation without a key")));
    }
    if version < 3 && atlas_format != AtlasFormat::Png {
        return Err(bad("file.exportWanim", format!("`atlasFormat` \"webp\" needs version 3: a version {version} file holds PNG atlases")));
    }
    let ranges = animation_ranges(comp, p)?;
    if work_area && !ranges.is_empty() {
        return Err(bad("file.exportWanim", "`workArea` and `animations` both say which frames to export: pass one of them"));
    }
    let layers = match p.get("layers").filter(|v| !v.is_null()) {
        None => true,
        Some(v) => v.as_bool().ok_or_else(|| bad("file.exportWanim", "`layers` must be true or false"))?,
    };
    // How frames are described: version 1 as one picture each, version 2 as layers, version 3
    // either way.
    let layered = match version {
        1 => false,
        2 => true,
        _ => layers,
    };
    let (start, end) = if work_area { comp.work_area } else { (effectcraft_time::Tick(0), comp.duration) };
    let key = key(s, p, BUILT_IN_KEY)?;
    let opts = RenderOpts { scale, backend: Backend::Cpu, nested_switches: s.prefs.general.switches_affect_nested_comps, ..Default::default() };
    let cache = LayerCache::default();
    let mut r = Renderer::new(&s.project, s.footage.as_ref(), opts);
    r.expr = s.expr.as_deref();
    r.cache = Some(&cache);
    let settings = Settings { version: if layered { 2 } else { 1 }, every_nth, start, end, tolerance: Tolerance::default() };
    let nonces = Nonces::random().map_err(err)?;
    let (anim, report, written) = if version == 3 {
        let (anim, clips, report) = effectcraft_wanim::plan::build_animations(&r, cid, &settings, &ranges, &mut |_, _| true).map_err(err)?;
        let written = effectcraft_wanim::write_animations(&anim, &clips, &AnimationsOptions { key, atlas_size, atlas_format, nonces }).map_err(err)?;
        (anim, report, written)
    } else {
        let (anim, report) = effectcraft_wanim::plan::build(&r, cid, &settings, &mut |_, _| true).map_err(err)?;
        let written = effectcraft_wanim::write(&anim, &WriteOptions { key, atlas_size: atlas_size.unwrap_or(max_atlas), nonces }).map_err(err)?;
        (anim, report, written)
    };
    s.write_output(&path, &written.file)?;
    let mb = written.file.len() as f64 / 1048576.0;
    s.toast(format!("Exported .wanim to {path} ({} frames, {mb:.1} MB)", written.frame_count));
    let mut out = json!({
        "path": path,
        "bytes": written.file.len(),
        "version": version,
        "frames": written.frame_count,
        "fps": anim.fps,
        "width": anim.width,
        "height": anim.height,
        "sprites": written.sprites,
        "atlases": written.atlases,
        "draws": written.draws,
        "gpuBytes": written.gpu_bytes,
        "customKey": str_p(p, "key").is_some(),
        "layeredFrames": report.layered,
        "flattenedFrames": report.flattened,
        "emptyFrames": report.empty,
    });
    if let (3, Some(m)) = (version, out.as_object_mut()) {
        // Per animation: key, frameCount, loop and the atlases it draws from.
        m.insert("animations".into(), written.manifest.get("animations").cloned().unwrap_or(Value::Null));
        m.insert("lists".into(), written.manifest.pointer("/scene/lists").cloned().unwrap_or(Value::Null));
        m.insert("atlasFormat".into(), json!(atlas_format.name()));
        m.insert("layers".into(), json!(layered));
    }
    Ok(out)
}

fn set_wanim_key(s: &mut Session, p: &Value) -> Result<Value> {
    let hex = str_p(p, "key").ok_or_else(|| bad("file.setWanimKey", "missing `key`"))?;
    // Checked before it is kept; the key itself is never echoed back.
    Key::official(hex).map_err(err)?;
    let cfg = s.config.clone().ok_or_else(|| EngineError::Other("this session has no settings folder to keep the key in".into()))?;
    cfg.write(OFFICIAL_KEY_FILE, hex.trim()).map_err(|e| EngineError::Other(format!("cannot write {OFFICIAL_KEY_FILE}: {e}")))?;
    Ok(json!({"saved": OFFICIAL_KEY_FILE}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "file.exportWanim",
            "WANIM...",
            ["File", "Export"],
            None,
            "{comp?, path (.wanim), version?: 3 | 2 | 1 (default 3: animations by key; 2: layers; 1: a sprite per frame), layers?: bool (version 3: frames as layers, default true; false: a picture per frame), animations?: [{key, firstFrame, lastFrame, loop?}] | \"markers\" (version 3; default one animation `main`), atlasFormat?: \"png\" | \"webp\" (version 3; default png), scale?: number, everyNthFrame?: int, workArea?: bool, atlasSize?: int (256-4096), key?: 64 hex characters (custom key; default the official key from the settings folder)}",
            has_comp,
            export_wanim
        ),
        cmd!("file.setWanimKey", "Set WANIM Official Key", [], None, "{key: 64 hex characters}", always, set_wanim_key),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Key, OFFICIAL_KEY_FILE, key};
    use crate::Session;
    use crate::config::MemoryConfig;

    const BUILT_IN: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SAVED: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const CUSTOM: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    #[test]
    fn a_custom_key_comes_first_then_the_saved_one_then_the_built_in_one() {
        let cfg = std::sync::Arc::new(MemoryConfig::default());
        let mut s = Session { config: Some(cfg.clone()), ..Default::default() };
        assert!(key(&s, &json!({}), None).is_err(), "no key anywhere");
        assert_eq!(key(&s, &json!({}), Some(BUILT_IN)).unwrap(), Key::official(BUILT_IN).unwrap());
        assert!(key(&s, &json!({}), Some("  ")).is_err(), "an empty built-in key is none");
        crate::config::ConfigStore::write(cfg.as_ref(), OFFICIAL_KEY_FILE, SAVED).unwrap();
        assert_eq!(key(&s, &json!({}), Some(BUILT_IN)).unwrap(), Key::official(SAVED).unwrap());
        assert_eq!(key(&s, &json!({"key": CUSTOM}), Some(BUILT_IN)).unwrap(), Key::custom(CUSTOM).unwrap());
        // A bad saved key is an error, not a silent fall back to the built-in one.
        crate::config::ConfigStore::write(cfg.as_ref(), OFFICIAL_KEY_FILE, "nope").unwrap();
        assert!(key(&s, &json!({}), Some(BUILT_IN)).is_err());
        // Without a settings folder the built-in key still works.
        s.config = None;
        assert_eq!(key(&s, &json!({}), Some(BUILT_IN)).unwrap(), Key::official(BUILT_IN).unwrap());
    }
}
