//! File ▸ Export ▸ WANIM… through the command registry.

use std::sync::Arc;

use serde_json::json;

use crate::Session;
use crate::config::MemoryConfig;

const KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("effectcraft-wanim-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

fn session() -> Session {
    let mut s = Session { config: Some(Arc::new(MemoryConfig::default())), ..Default::default() };
    s.execute("comp.new", json!({"name": "Badge", "width": 120, "height": 80, "frameRate": 30, "duration": 1})).unwrap();
    s.execute("layer.newSolid", json!({"color": "#ff8000", "width": 40, "height": 30})).unwrap();
    s.execute("prop.addKey", json!({"layer": "#1", "path": "transform/position", "time": 0.0, "value": [20.0, 40.0]})).unwrap();
    s.execute("prop.addKey", json!({"layer": "#1", "path": "transform/position", "time": 0.9, "value": [100.0, 40.0]})).unwrap();
    s.execute("prop.addKey", json!({"layer": "#1", "path": "transform/opacity", "time": 0.0, "value": 100.0})).unwrap();
    s.execute("prop.addKey", json!({"layer": "#1", "path": "transform/opacity", "time": 0.9, "value": 30.0})).unwrap();
    s
}

fn key_bytes(hex: &str) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    k
}

#[test]
fn export_needs_the_official_key_or_a_custom_one() {
    let mut s = session();
    let path = tmp("nokey.wanim");
    let e = s.execute_checked("file.exportWanim", json!({"path": path})).unwrap_err().to_string();
    assert!(e.contains("file.setWanimKey"), "{e}");
    assert!(!std::path::Path::new(&path).exists());
    assert!(s.execute_checked("file.setWanimKey", json!({"key": "1234"})).is_err());
    let r = s.execute_checked("file.setWanimKey", json!({"key": KEY})).unwrap();
    assert!(!r.to_string().contains(KEY), "the key is not echoed back");
    assert_eq!(s.config.as_ref().unwrap().read("wanim_official_key.txt").as_deref(), Some(KEY));
}

#[test]
fn exports_layers_with_the_official_key() {
    let mut s = session();
    s.execute_checked("file.setWanimKey", json!({"key": KEY})).unwrap();
    let path = tmp("badge.wanim");
    let r = s.execute_checked("file.exportWanim", json!({"path": path})).unwrap();
    assert_eq!(r["version"], 2);
    assert_eq!(r["frames"], 30);
    assert_eq!(r["layeredFrames"], 30, "{r}");
    assert_eq!(r["sprites"], 1, "the moving, fading solid is stored once");
    assert_eq!(r["customKey"], false);
    let bytes = std::fs::read(&path).unwrap();
    let h = effectcraft_wanim::read_header(&bytes).unwrap();
    assert_eq!((h.version, h.custom_key), (2, false));
    let o = effectcraft_wanim::open(&bytes, &key_bytes(KEY)).unwrap();
    assert_eq!(o.manifest["compositionId"], "Badge");
    assert_eq!((o.width, o.height), (120, 80));
}

#[test]
fn exports_version_1_with_a_custom_key_at_half_size() {
    let mut s = session();
    let custom = "ff".repeat(32);
    let path = tmp("badge-v1.wanim");
    let r = s.execute_checked("file.exportWanim", json!({"path": path, "version": 1, "key": custom, "scale": 0.5, "everyNthFrame": 2})).unwrap();
    assert_eq!(r["frames"], 15);
    assert_eq!(r["fps"], 15.0);
    let bytes = std::fs::read(&path).unwrap();
    assert!(effectcraft_wanim::read_header(&bytes).unwrap().custom_key);
    let o = effectcraft_wanim::open(&bytes, &key_bytes(&custom)).unwrap();
    assert_eq!((o.width, o.height), (60, 40));
    assert!(matches!(o.content, effectcraft_wanim::Content::V1 { .. }));
    // Out-of-range and unknown parameters are errors, not panics.
    for bad in [
        json!({"path": path, "version": 3, "key": custom}),
        json!({"path": path, "scale": -1, "key": custom}),
        json!({"path": path, "everyNthFrame": 0, "key": custom}),
        json!({"path": path, "bogus": 1}),
    ] {
        assert!(s.execute_checked("file.exportWanim", bad.clone()).is_err(), "{bad}");
    }
}

#[test]
fn wanim_menu_entry_exists() {
    let entries = crate::menus::entries();
    let find = |cmd: &str| entries.iter().find(|(_, e)| e.command == cmd).map(|(p, e)| (p.join(" > "), e.label.clone()));
    assert_eq!(find("file.exportWanim"), Some(("File > Export".into(), "WANIM...".into())));
    assert_eq!(find("file.setWanimKey"), None, "the key is set by agents and scripts, not from a menu");
}
