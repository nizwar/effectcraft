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
    let r = s.execute_checked("file.exportWanim", json!({"path": path, "version": 2})).unwrap();
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
        json!({"path": path, "version": 4, "key": custom}),
        json!({"path": path, "version": 0, "key": custom}),
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

fn opened(path: &str) -> effectcraft_wanim::Opened {
    effectcraft_wanim::open(&std::fs::read(path).unwrap(), &key_bytes(KEY)).unwrap()
}

fn scene(o: &effectcraft_wanim::Opened) -> &effectcraft_wanim::Scene {
    let effectcraft_wanim::Content::V3 { scene, .. } = &o.content else { panic!("version 3") };
    scene
}

#[test]
fn exports_version_3_by_default_with_one_animation() {
    let mut s = session();
    s.execute_checked("file.setWanimKey", json!({"key": KEY})).unwrap();
    let path = tmp("badge-v3.wanim");
    let r = s.execute_checked("file.exportWanim", json!({"path": path})).unwrap();
    assert_eq!(r["version"], 3);
    assert_eq!((&r["frames"], &r["layeredFrames"], &r["layers"], &r["atlasFormat"]), (&json!(30), &json!(30), &json!(true), &json!("smallest")), "{r}");
    assert_eq!(r["animations"], json!([{"key": "main", "frameCount": 30, "loop": true, "atlases": [0]}]));
    assert_eq!(r["sprites"], 1, "the moving, fading solid is stored once");
    // The solid holds still from 0.9 s on: frames 27, 28 and 29 are one list.
    assert_eq!((&r["draws"], &r["lists"]), (&json!(28), &json!(28)), "{r}");
    let bytes = std::fs::read(&path).unwrap();
    let h = effectcraft_wanim::read_header(&bytes).unwrap();
    assert_eq!((h.version, h.custom_key), (3, false));
    let o = opened(&path);
    assert_eq!(o.manifest["compositionId"], "Badge");
    assert_eq!((o.width, o.height, o.frame_count()), (120, 80, 30));
    assert_eq!(o.animations.len(), 1);
    assert_eq!(scene(&o).frames[27..], [27, 27, 27]);
    // The same frames as a picture each.
    let r = s.execute_checked("file.exportWanim", json!({"path": tmp("badge-v3-pictures.wanim"), "layers": false})).unwrap();
    assert_eq!((&r["version"], &r["layers"], &r["layeredFrames"], &r["frames"]), (&json!(3), &json!(false), &json!(0), &json!(30)), "{r}");
    // Versions 1 and 2 describe frames in their one way, whatever `layers` says.
    let r = s.execute_checked("file.exportWanim", json!({"path": tmp("badge-v2-layers.wanim"), "version": 2, "layers": false})).unwrap();
    assert_eq!((&r["version"], &r["layeredFrames"]), (&json!(2), &json!(30)));
    assert!(r.get("animations").is_none());
}

#[test]
fn exports_animations_by_key() {
    let mut s = session();
    s.execute_checked("file.setWanimKey", json!({"key": KEY})).unwrap();
    let path = tmp("badge-animations.wanim");
    let animations = json!([
        {"key": "transition_in", "firstFrame": 0, "lastFrame": 9},
        {"key": "idle", "firstFrame": 10, "lastFrame": 19, "loop": true},
        {"key": "transition_out", "firstFrame": 15, "lastFrame": 29, "loop": false},
    ]);
    let r = s.execute_checked("file.exportWanim", json!({"path": path, "animations": animations})).unwrap();
    assert_eq!(r["frames"], 35);
    assert_eq!(
        r["animations"],
        json!([
            {"key": "transition_in", "frameCount": 10, "loop": false, "atlases": [0]},
            {"key": "idle", "frameCount": 10, "loop": true, "atlases": [0]},
            {"key": "transition_out", "frameCount": 15, "loop": false, "atlases": [0]},
        ])
    );
    assert_eq!(r["sprites"], 1, "all three draw the one solid");
    let o = opened(&path);
    let (idle, out) = (o.animation("idle").unwrap(), o.animation("transition_out").unwrap());
    assert_eq!((idle.start, idle.frame_count, idle.looped), (10, 10, true));
    // Comp frames 15 to 19 are in both: the same lists.
    let frames = &scene(&o).frames;
    assert_eq!(frames[idle.start + 5..idle.start + 10], frames[out.start..out.start + 5]);
    assert_eq!(r["lists"], 28, "{r}");
    // Every second frame of each range, as WebP.
    let path = tmp("badge-animations-webp.wanim");
    let r = s.execute_checked("file.exportWanim", json!({"path": path, "animations": animations, "everyNthFrame": 2, "atlasFormat": "webp"})).unwrap();
    assert_eq!((&r["frames"], &r["fps"], &r["atlasFormat"]), (&json!(18), &json!(15.0), &json!("webp")));
    let o = opened(&path);
    assert_eq!(o.animations.iter().map(|a| a.frame_count).collect::<Vec<_>>(), vec![5, 5, 8]);
    assert_eq!(o.manifest["atlases"][0]["format"], "webp");
}

#[test]
fn exports_comp_markers_as_animations() {
    let mut s = session();
    s.execute_checked("file.setWanimKey", json!({"key": KEY})).unwrap();
    let path = tmp("badge-markers.wanim");
    let e = s.execute_checked("file.exportWanim", json!({"path": path, "animations": "markers"})).unwrap_err().to_string();
    assert!(e.contains("comp markers that have a duration"), "{e}");
    // Added out of order; a marker without a duration or without a comment is not an animation.
    s.execute_checked("markers.set", json!({"new": true, "time": 0.3, "duration": 0.4, "comment": "idle loop"})).unwrap();
    s.execute_checked("markers.set", json!({"new": true, "time": 0.0, "duration": 0.3, "comment": "transition_in"})).unwrap();
    s.execute_checked("markers.set", json!({"new": true, "time": 0.7, "duration": 5.0, "comment": "transition_out"})).unwrap();
    s.execute_checked("markers.set", json!({"new": true, "time": 0.5, "comment": "a note"})).unwrap();
    s.execute_checked("markers.set", json!({"new": true, "time": 0.2, "duration": 0.2})).unwrap();
    let r = s.execute_checked("file.exportWanim", json!({"path": path, "animations": "markers"})).unwrap();
    // 0.3 s is frame 9, 0.7 s frame 21, and the last marker ends with the comp.
    assert_eq!(
        r["animations"],
        json!([
            {"key": "transition_in", "frameCount": 9, "loop": false, "atlases": [0]},
            {"key": "idle", "frameCount": 12, "loop": true, "atlases": [0]},
            {"key": "transition_out", "frameCount": 9, "loop": false, "atlases": [0]},
        ])
    );
    assert_eq!(r["frames"], 30);
    assert_eq!(opened(&path).animation("idle").map(|a| a.start), Some(9));
    // A marker whose comment is no key says which marker it is.
    s.execute_checked("markers.set", json!({"new": true, "time": 0.1, "duration": 0.1, "comment": "fade out slowly"})).unwrap();
    let e = s.execute_checked("file.exportWanim", json!({"path": path, "animations": "markers"})).unwrap_err().to_string();
    assert!(e.contains("0.100 s") && e.contains("fade out slowly"), "{e}");
    s.execute_checked("markers.set", json!({"at": 0.1, "comment": "idle"})).unwrap();
    let e = s.execute_checked("file.exportWanim", json!({"path": path, "animations": "markers"})).unwrap_err().to_string();
    assert!(e.contains("`idle` is used twice"), "{e}");
}

#[test]
fn bad_animation_parameters_are_errors() {
    let mut s = session();
    s.execute_checked("file.setWanimKey", json!({"key": KEY})).unwrap();
    let path = tmp("badge-bad.wanim");
    let one = |key: &str, first: i64, last: i64| json!([{"key": key, "firstFrame": first, "lastFrame": last}]);
    for (params, want) in [
        (json!({"animations": one("two words", 0, 9)}), "animation key is 1 to 64"),
        (json!({"animations": one("", 0, 9)}), "animation key is 1 to 64"),
        (json!({"animations": one(&"k".repeat(65), 0, 9)}), "animation key is 1 to 64"),
        (json!({"animations": [{"key": "a", "firstFrame": 0, "lastFrame": 4}, {"key": "a", "firstFrame": 5, "lastFrame": 9}]}), "`a` is used twice"),
        (json!({"animations": one("idle", 0, 30)}), "`idle` is frames 0 to 30, which is not a range inside the composition (frames 0 to 29)"),
        (json!({"animations": one("idle", 9, 8)}), "`idle` is frames 9 to 8"),
        (json!({"animations": one("idle", -1, 8)}), "`idle` is frames -1 to 8"),
        (json!({"animations": [{"key": "idle", "firstFrame": 0}]}), "`animations[0].lastFrame` (animation `idle`) must be a whole frame number"),
        (json!({"animations": [{"key": "idle", "firstFrame": 0.5, "lastFrame": 3}]}), "`animations[0].firstFrame`"),
        (json!({"animations": [{"firstFrame": 0, "lastFrame": 3}]}), "`animations[0]` has no `key`"),
        (json!({"animations": [{"key": "idle", "firstFrame": 0, "lastFrame": 3, "loop": "yes"}]}), "`animations[0].loop`"),
        (json!({"animations": [{"key": "idle", "firstFrame": 0, "lastFrame": 3, "start": 0}]}), "unknown field `start`"),
        (json!({"animations": ["idle"]}), "`animations[0]` is not"),
        (json!({"animations": []}), "holds 1 to 256 animations"),
        (json!({"animations": "chapters"}), "or \"markers\""),
        (json!({"animations": 3}), "or \"markers\""),
        (json!({"animations": one("idle", 0, 9), "workArea": true}), "`workArea` and `animations`"),
        (json!({"animations": one("idle", 0, 9), "version": 2}), "`animations` needs version 3"),
        (json!({"animations": "markers", "version": 1}), "needs version 3"),
        (json!({"atlasFormat": "webp", "version": 2}), "`atlasFormat` \"webp\" needs version 3"),
        (json!({"atlasFormat": "gif"}), "`atlasFormat` is"),
        (json!({"layers": "yes"}), "layers"),
    ] {
        let mut p = params.clone();
        p["path"] = json!(path);
        let e = s.execute_checked("file.exportWanim", p).unwrap_err().to_string();
        assert!(e.contains(want), "{params}: {e}");
        assert!(!std::path::Path::new(&path).exists(), "{params}: nothing is written");
    }
    // An absent list (the dialog's "one animation") is the default.
    let r = s.execute_checked("file.exportWanim", json!({"path": path, "animations": null, "atlasFormat": null, "atlasSize": null})).unwrap();
    assert_eq!(r["animations"][0]["key"], "main");
}
