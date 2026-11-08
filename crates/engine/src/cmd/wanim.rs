//! File ▸ Export ▸ WANIM… (`effectcraft-wanim`): the comp as an encrypted `.wanim` sprite
//! animation, version 2 (layers) by default.
//!
//! Files use the official key unless the command is given a custom `key`. The official key is
//! never part of EffectCraft's source: it is read from [`OFFICIAL_KEY_FILE`] in the settings
//! folder (`file.setWanimKey` writes it), or else built in when the binary was compiled with
//! `EFFECTCRAFT_WANIM_OFFICIAL_KEY` set ([`BUILT_IN_KEY`]).

use effectcraft_render::{Backend, LayerCache, RenderOpts, Renderer};
use effectcraft_wanim::plan::{Settings, Tolerance};
use effectcraft_wanim::{Key, Nonces, WriteOptions};
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

fn export_wanim(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("file.exportWanim", "missing `path`"))?.to_string();
    s.refuse_export(&path)?;
    let cid = comp_id(s, p)?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let version = int_p(p, "version", 1, 2, 2)? as u16;
    let every_nth = int_p(p, "everyNthFrame", 1, 1000, 1)?;
    let atlas_size = int_p(p, "atlasSize", 256, effectcraft_wanim::atlas::MAX_ATLAS, effectcraft_wanim::atlas::MAX_ATLAS)?;
    let scale = f_p(p, "scale").unwrap_or(1.0);
    if !(scale.is_finite() && scale > 0.0 && scale <= 4.0) {
        return Err(bad("file.exportWanim", "`scale` must be above 0 and at most 4"));
    }
    let (start, end) = if b_p(p, "workArea").unwrap_or(false) { comp.work_area } else { (effectcraft_time::Tick(0), comp.duration) };
    let key = key(s, p, BUILT_IN_KEY)?;
    let opts = RenderOpts { scale, backend: Backend::Cpu, nested_switches: s.prefs.general.switches_affect_nested_comps, ..Default::default() };
    let cache = LayerCache::default();
    let mut r = Renderer::new(&s.project, s.footage.as_ref(), opts);
    r.expr = s.expr.as_deref();
    r.cache = Some(&cache);
    let settings = Settings { version, every_nth, start, end, tolerance: Tolerance::default() };
    let (anim, report) = effectcraft_wanim::plan::build(&r, cid, &settings, &mut |_, _| true).map_err(err)?;
    let written = effectcraft_wanim::write(&anim, &WriteOptions { key, atlas_size, nonces: Nonces::random().map_err(err)? }).map_err(err)?;
    s.write_output(&path, &written.file)?;
    let mb = written.file.len() as f64 / 1048576.0;
    s.toast(format!("Exported .wanim to {path} ({} frames, {mb:.1} MB)", written.frame_count));
    Ok(json!({
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
    }))
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
            "{comp?, path (.wanim), version?: 1 | 2 (default 2: layers), scale?: number, everyNthFrame?: int, workArea?: bool, atlasSize?: int (256-4096), key?: 64 hex characters (custom key; default the official key from the settings folder)}",
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
