use effectcraft_color::{BlendMode, Label};
use effectcraft_keyframe::{Keyframe, Value};
use effectcraft_project::build;
use effectcraft_project::{Comp, ItemId, ItemKind, Layer, LayerSource, Project, Solid};
use effectcraft_render::{Backend, LayerCache, NoFootage, RenderOpts, Renderer};
use effectcraft_time::{FrameRate, Tick};
use sha2::Digest;

use crate::plan::{Settings, Tolerance, build as plan_build};
use crate::{Animation, Bitmap, Content, Draw, Frames, Key, Nonces, Placed, WriteOptions, compose, open, read_header, write};

const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn key_bytes() -> [u8; 32] {
    crate::container::parse_hex_key(KEY_HEX).unwrap()
}

fn opts(custom: bool) -> WriteOptions {
    let key = if custom { Key::custom(KEY_HEX) } else { Key::official(KEY_HEX) }.unwrap();
    WriteOptions { key, atlas_size: 4096, nonces: Nonces { salt: [7; 16], nonce_prefix: [9; 8] } }
}

fn square(w: u32, h: u32, rgba: [u8; 4]) -> Bitmap {
    Bitmap { width: w, height: h, rgba: rgba.repeat((w * h) as usize) }
}

fn bordered(w: u32, h: u32, rgba: [u8; 4]) -> Bitmap {
    let mut b = square(w + 2, h + 2, [0; 4]);
    for y in 1..=h {
        for x in 1..=w {
            let i = ((y * (w + 2) + x) * 4) as usize;
            b.rgba[i..i + 4].copy_from_slice(&rgba);
        }
    }
    b
}

#[test]
fn keys_are_64_hex_characters() {
    assert!(Key::custom(KEY_HEX).unwrap().is_custom());
    assert!(!Key::official(&format!("  {KEY_HEX}\n")).unwrap().is_custom());
    assert!(Key::custom("abc").is_err());
    assert!(Key::custom(&"zz".repeat(32)).is_err());
    // A multi-byte character must not be split mid-way.
    assert!(Key::custom(&format!("é{}", &KEY_HEX[2..])).is_err());
    assert!(!format!("{:?}", Key::custom(KEY_HEX).unwrap()).contains("0001"), "a key is never printed");
}

fn v1_animation() -> Animation {
    Animation {
        width: 40,
        height: 30,
        fps: 30.0,
        composition_id: Some("ball".into()),
        opaque: false,
        bitmaps: vec![square(10, 10, [255, 0, 0, 255]), square(5, 5, [0, 0, 255, 128]), square(3, 3, [1, 2, 3, 4])],
        frames: Frames::Sprites(vec![
            Some(Placed { bitmap: 1, dx: 4, dy: 5 }),
            Some(Placed { bitmap: 0, dx: 0, dy: 0 }),
            None,
            Some(Placed { bitmap: 1, dx: 4, dy: 5 }),
        ]),
    }
}

#[test]
fn version_1_round_trips() {
    let w = write(&v1_animation(), &opts(false)).unwrap();
    let h = read_header(&w.file).unwrap();
    assert_eq!((h.version, h.custom_key), (1, false));
    assert_eq!(w.sprites, 2, "the unused bitmap is dropped");
    let o = open(&w.file, &key_bytes()).unwrap();
    let Content::V1 { sprites, frames } = &o.content else { panic!("v1") };
    assert_eq!(frames, &vec![0, 1, -1, 0]);
    // Sprites are packed in the order of first use, 2 pixels from the edge and apart.
    assert_eq!(sprites[0], [0, 2, 2, 5, 5, 4, 5]);
    assert_eq!(sprites[1], [0, 9, 2, 10, 10, 0, 0]);
    assert_eq!((o.atlases[0].width, o.atlases[0].height), (21, 14));
    assert_eq!(o.manifest["gpuBytes"], 21 * 14 * 4);
    assert_eq!(o.manifest["compositionId"], "ball");
    let f0 = compose::frame(&o, 0).to_rgba8();
    assert_eq!(&f0[((5 * 40 + 4) * 4) as usize..][..4], &[0, 0, 255, 128]);
    assert!(compose::frame(&o, 2).to_rgba8().iter().all(|&v| v == 0));
}

fn v2_animation() -> Animation {
    Animation {
        width: 64,
        height: 48,
        fps: 12.0,
        composition_id: None,
        opaque: false,
        bitmaps: vec![bordered(12, 12, [200, 10, 10, 255]), bordered(20, 6, [10, 200, 10, 255])],
        frames: Frames::Layers(vec![
            vec![Draw { sprite: 0, matrix: [1.0, 0.0, 0.0, 1.0, 3.0, 3.0], alpha: 1.0 }],
            vec![
                Draw { sprite: 0, matrix: [0.866, 0.5, -0.5, 0.866, 13.4, 6.4], alpha: 1.0 },
                Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, 9.0, 33.0], alpha: 0.5 },
            ],
            vec![],
        ]),
    }
}

#[test]
fn version_2_round_trips_with_a_custom_key() {
    let w = write(&v2_animation(), &opts(true)).unwrap();
    let h = read_header(&w.file).unwrap();
    assert_eq!((h.version, h.custom_key), (2, true));
    assert_eq!(w.draws, 3);
    let o = open(&w.file, &key_bytes()).unwrap();
    let Content::V2 { sprites, frames } = &o.content else { panic!("v2") };
    assert_eq!(sprites, &vec![[0, 2, 2, 14, 14], [0, 18, 2, 22, 8]]);
    assert_eq!(
        frames,
        &match v2_animation().frames {
            Frames::Layers(f) => f,
            Frames::Sprites(_) => panic!("v2"),
        }
    );
    assert_eq!(o.manifest["timeline"]["draws"], 3);
    // Frame 0: the red square, 1:1 at (4, 4) (its border is at 3, 3).
    let f0 = compose::frame(&o, 0).to_rgba8();
    assert_eq!(&f0[((4 * 64 + 4) * 4) as usize..][..4], &[200, 10, 10, 255]);
    assert_eq!(f0[((3 * 64 + 3) * 4 + 3) as usize], 0);
    // Frame 1: the green bar at half opacity.
    let f1 = compose::frame(&o, 1).to_rgba8();
    assert_eq!(f1[((36 * 64 + 20) * 4 + 3) as usize], 128);
}

#[test]
fn a_wrong_key_or_a_changed_byte_is_an_error() {
    let w = write(&v2_animation(), &opts(false)).unwrap();
    assert!(open(&w.file, &[1; 32]).is_err());
    for i in [9, 20, 45, w.file.len() - 1] {
        let mut f = w.file.clone();
        f[i] ^= 1;
        assert!(open(&f, &key_bytes()).is_err(), "byte {i}");
    }
}

#[test]
fn damaged_files_never_panic() {
    let w = write(&v2_animation(), &opts(false)).unwrap();
    for n in 0..w.file.len() {
        let _ = open(&w.file[..n], &key_bytes());
    }
    let _ = open(&[], &key_bytes());
    let mut huge = w.file.clone();
    huge[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(open(&huge, &key_bytes()).is_err());
    assert!(crate::container::decode_timeline(&[0; 8], 1, 0).is_ok());
    assert!(crate::container::decode_timeline(&[0; 8], 1, 1).is_err());
    assert!(crate::container::decode_timeline(&[5, 0, 0, 0, 0, 0, 0, 0], 1, 0).is_err(), "frameStart must start at 0");
}

#[test]
fn bad_animations_are_errors() {
    let mut a = v2_animation();
    if let Frames::Layers(f) = &mut a.frames {
        f[0][0].sprite = 9;
    }
    assert!(write(&a, &opts(false)).is_err());
    let mut a = v2_animation();
    if let Frames::Layers(f) = &mut a.frames {
        f[0][0].matrix[0] = f32::NAN;
    }
    assert!(write(&a, &opts(false)).is_err());
    let mut a = v1_animation();
    a.bitmaps[0] = square(5000, 2, [1; 4]);
    if let Frames::Sprites(f) = &mut a.frames {
        f[0] = Some(Placed { bitmap: 0, dx: 0, dy: 0 });
    }
    assert!(matches!(write(&a, &opts(false)), Err(crate::Error::TooLarge(_))));
}

#[test]
fn atlases_fill_up_and_continue_on_the_next_page() {
    let sizes = vec![(60, 60); 5];
    let (pages, places) = crate::atlas::shelf_pack(&sizes, 128).unwrap();
    // 2 + 60 + 2 + 60 + 2 = 126: two by two on a 128 page.
    assert_eq!(pages.len(), 2);
    assert_eq!(places.iter().map(|p| p.page).collect::<Vec<_>>(), vec![0, 0, 0, 0, 1]);
    assert_eq!((places[3].x, places[3].y), (64, 64));
    assert!(pages.iter().all(|p| p.width <= 128 && p.height <= 128));
}

// ---- Exporting compositions ----

fn s(x: f64) -> Tick {
    Tick::from_seconds_f64(x)
}

fn comp_with(layers: impl FnOnce(&mut Project, &Comp) -> Vec<Layer>) -> (Project, ItemId) {
    let mut p = Project::default();
    let comp = Comp::new(80, 60, FrameRate::FPS_30, s(1.0));
    let cid = p.add_item("Logo", Label::Sandstone, None, ItemKind::Comp(comp.into()));
    let c = p.comp(cid).unwrap().clone();
    let ls = layers(&mut p, &c);
    p.comp_mut(cid).unwrap().layers.extend(ls);
    (p, cid)
}

fn solid(p: &mut Project, c: &Comp, color: [f32; 3], w: u32, h: u32) -> Layer {
    let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color, width: w, height: h, pixel_aspect: 1.0 }));
    build::layer(p, c, "Solid", LayerSource::Solid { item: sid }, (w, h), None)
}

fn animate(l: &mut Layer, prop: &str, a: Value, b: Value) {
    let pr = l.props.sub_mut("transform").unwrap().get_mut(prop).unwrap();
    pr.keys = vec![Keyframe::new(s(0.0), a), Keyframe::new(s(0.9), b)];
}

fn export(p: &Project, cid: ItemId, version: u16) -> (crate::Opened, crate::plan::Report, Vec<Vec<u8>>) {
    let cache = LayerCache::default();
    let mut r = Renderer::new(p, &NoFootage, RenderOpts { backend: Backend::Cpu, ..Default::default() });
    r.cache = Some(&cache);
    let c = p.comp(cid).unwrap();
    let settings = Settings { version, every_nth: 1, start: Tick(0), end: c.duration, tolerance: Tolerance::default() };
    let (anim, report) = plan_build(&r, cid, &settings, &mut |_, _| true).unwrap();
    let rendered = (0..anim.frame_count()).map(|i| r.comp_frame(cid, c.frame_rate.tick_of(i as i64)).to_rgba8()).collect();
    let w = write(&anim, &opts(false)).unwrap();
    (open(&w.file, &key_bytes()).unwrap(), report, rendered)
}

/// Largest premultiplied channel difference and the share of pixels off by more than 6.
fn diff(a: &[u8], b: &[u8]) -> (i32, f64) {
    let pm = |p: &[u8], c: usize| if c == 3 { p[3] as i32 } else { (p[c] as i32 * p[3] as i32 + 127) / 255 };
    let mut worst = 0;
    let mut off = 0;
    for (x, y) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()) {
        let d = (0..4).map(|c| (pm(x, c) - pm(y, c)).abs()).max().unwrap();
        worst = worst.max(d);
        off += usize::from(d > 6);
    }
    (worst, off as f64 / (a.len() / 4) as f64)
}

fn moving_logo() -> (Project, ItemId) {
    comp_with(|p, c| {
        let mut bar = solid(p, c, [0.1, 0.8, 0.3], 30, 8);
        animate(&mut bar, "position", Value::Vec3([10.0, 50.0, 0.0]), Value::Vec3([70.0, 50.0, 0.0]));
        animate(&mut bar, "opacity", Value::Scalar(100.0), Value::Scalar(20.0));
        let mut logo = solid(p, c, [0.9, 0.2, 0.1], 20, 20);
        animate(&mut logo, "rotation", Value::Scalar(0.0), Value::Scalar(90.0));
        animate(&mut logo, "scale", Value::Vec3([100.0, 100.0, 100.0]), Value::Vec3([150.0, 60.0, 100.0]));
        vec![logo, bar]
    })
}

#[test]
fn moving_layers_are_stored_once_and_drawn_with_matrices() {
    let (p, cid) = moving_logo();
    let (o, report, rendered) = export(&p, cid, 2);
    assert_eq!(report.frames, 30);
    assert_eq!(report.layered, 30, "{report:?}");
    let Content::V2 { sprites, frames } = &o.content else { panic!("v2") };
    assert_eq!(sprites.len(), 2, "two layers, each stored once");
    assert!(frames.iter().all(|f| f.len() == 2));
    assert_eq!(o.manifest["compositionId"], "Logo");
    assert_eq!(o.manifest["fps"], 30.0);
    for (f, want) in rendered.iter().enumerate() {
        let (worst, share) = diff(&compose::frame(&o, f).to_rgba8(), want);
        assert!(worst <= 40 && share <= 0.002, "frame {f}: worst {worst}, {share}");
    }
}

#[test]
fn frames_a_layer_list_cannot_express_are_stored_as_pictures() {
    let (mut p, cid) = moving_logo();
    p.comp_mut(cid).unwrap().layers[0].blend_mode = BlendMode::Multiply;
    let (o, report, rendered) = export(&p, cid, 2);
    assert_eq!(report.layered, 0);
    assert_eq!(report.flattened.get("blend mode"), Some(&30));
    for (f, want) in rendered.iter().enumerate() {
        assert_eq!(diff(&compose::frame(&o, f).to_rgba8(), want).0, 0, "frame {f} is the rendered picture");
    }
}

#[test]
fn version_1_shares_sprites_between_frames_that_look_the_same() {
    let (p, cid) = comp_with(|p, c| vec![solid(p, c, [0.2, 0.4, 0.9], 30, 30)]);
    let (o, report, rendered) = export(&p, cid, 1);
    assert_eq!(report.frames, 30);
    let Content::V1 { sprites, frames } = &o.content else { panic!("v1") };
    assert_eq!(sprites.len(), 1);
    assert!(frames.iter().all(|&f| f == 0));
    assert_eq!(diff(&compose::frame(&o, 0).to_rgba8(), &rendered[0]).0, 0);
    // Cropped to the solid: 30×30 at the comp's centre.
    assert_eq!(sprites[0][3..], [30, 30, 25, 15]);
}

#[test]
fn an_empty_span_or_unknown_version_is_an_error() {
    let (p, cid) = moving_logo();
    let r = Renderer::new(&p, &NoFootage, RenderOpts { backend: Backend::Cpu, ..Default::default() });
    let mut st = Settings { version: 2, every_nth: 1, start: s(0.5), end: s(0.5), tolerance: Tolerance::default() };
    assert!(plan_build(&r, cid, &st, &mut |_, _| true).is_err());
    st.end = s(1.0);
    st.version = 3;
    assert!(plan_build(&r, cid, &st, &mut |_, _| true).is_err());
    st.version = 2;
    assert!(matches!(plan_build(&r, cid, &st, &mut |_, _| false), Err(crate::Error::Cancelled)));
    st.every_nth = 4;
    let (anim, _) = plan_build(&r, cid, &st, &mut |_, _| true).unwrap();
    assert_eq!(anim.frame_count(), 4, "frames 15, 19, 23 and 27");
    assert_eq!(anim.fps, 7.5);
}

/// The format's conformance fixtures, when a local copy is available
/// (`EFFECTCRAFT_WANIM_FIXTURES=<folder with expected.json and the sample files>`).
#[test]
fn reads_the_conformance_fixtures() {
    let Ok(dir) = std::env::var("EFFECTCRAFT_WANIM_FIXTURES") else { return };
    let dir = std::path::Path::new(&dir);
    for (file, expected) in [("sample-custom-key.wanim", "expected.json"), ("sample-layers-custom-key.wanim", "expected-layers.json")] {
        let bytes = std::fs::read(dir.join(file)).unwrap();
        let exp: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join(expected)).unwrap()).unwrap();
        let key = crate::container::parse_hex_key(exp["customKey"].as_str().unwrap()).unwrap();
        let o = open(&bytes, &key).unwrap();
        assert_eq!(o.manifest, exp["manifest"], "{file}");
        // Atlas hashes are of the decrypted PNG blocks.
        let f = crate::container::OpenedFile::open(&bytes, &key).unwrap();
        for (i, a) in exp["manifest"]["atlases"].as_array().unwrap().iter().enumerate() {
            let png = f.block(&bytes, i as u32 + 1, a["offset"].as_u64().unwrap(), a["length"].as_u64().unwrap()).unwrap();
            assert_eq!(format!("{:x}", sha2::Sha256::digest(&png)), exp["atlasSha256"][i].as_str().unwrap(), "{file} atlas {i}");
        }
        if let Content::V2 { frames, .. } = &o.content {
            let want = exp["timeline"].as_array().unwrap();
            for (f, draws) in frames.iter().enumerate() {
                for (d, w) in draws.iter().zip(want[f].as_array().unwrap()) {
                    assert_eq!(d.sprite as u64, w["sprite"].as_u64().unwrap());
                    for (a, b) in d.matrix.iter().zip(w["matrix"].as_array().unwrap()) {
                        assert_eq!(*a as f64, b.as_f64().unwrap());
                    }
                }
                // Frame pixels: the reference renderer's PNGs, when present.
                if let Ok(png) = std::fs::read(dir.join(format!("layers-frame-{f}.png"))) {
                    let want = crate::atlas::decode_png(&png).unwrap();
                    let (worst, _) = diff(&compose::frame(&o, f).to_rgba8(), &want.rgba);
                    assert!(worst <= 1, "{file} frame {f}: worst {worst}");
                }
            }
        }
    }
}

/// Writes the moving logo with the official key for the format's own reader to check, when the
/// key and an output folder are given (`EFFECTCRAFT_WANIM_OFFICIAL_KEY`, `EFFECTCRAFT_WANIM_OUT`).
/// The key is never part of the repository.
#[test]
fn writes_files_for_the_formats_reader() {
    let (Ok(hex), Ok(out)) = (std::env::var("EFFECTCRAFT_WANIM_OFFICIAL_KEY"), std::env::var("EFFECTCRAFT_WANIM_OUT")) else { return };
    let (p, cid) = moving_logo();
    let mut r = Renderer::new(&p, &NoFootage, RenderOpts { backend: Backend::Cpu, ..Default::default() });
    let cache = LayerCache::default();
    r.cache = Some(&cache);
    let c = p.comp(cid).unwrap();
    for version in [1, 2] {
        let settings = Settings { version, every_nth: 1, start: Tick(0), end: c.duration, tolerance: Tolerance::default() };
        let (anim, _) = plan_build(&r, cid, &settings, &mut |_, _| true).unwrap();
        let w = write(&anim, &WriteOptions { key: Key::official(&hex).unwrap(), atlas_size: 4096, nonces: Nonces::random().unwrap() }).unwrap();
        std::fs::write(std::path::Path::new(&out).join(format!("logo-v{version}.wanim")), &w.file).unwrap();
        // The rendered frames, for comparison.
        for f in [0usize, 15, 29] {
            let img = r.comp_frame(cid, c.frame_rate.tick_of(f as i64));
            let png = crate::atlas::encode_png(img.width, img.height, &img.to_rgba8()).unwrap();
            std::fs::write(std::path::Path::new(&out).join(format!("logo-rendered-{f}.png")), png).unwrap();
        }
    }
}
