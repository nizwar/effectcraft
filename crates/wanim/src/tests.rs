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

/// Players draw an opaque file without blending: a layered frame over an opaque background must
/// not be marked opaque, or the layers above the background would replace it instead of blending.
#[test]
fn layered_frames_over_an_opaque_background_are_not_opaque() {
    let (p, cid) = comp_with(|p, c| {
        let mut bar = solid(p, c, [0.1, 0.8, 0.3], 30, 8);
        animate(&mut bar, "opacity", Value::Scalar(100.0), Value::Scalar(40.0));
        vec![bar, solid(p, c, [1.0, 1.0, 1.0], 80, 60)]
    });
    let (o, report, _) = export(&p, cid, 2);
    assert_eq!(report.layered, 30);
    assert_eq!(o.manifest["opaque"], false);
    // As pictures (a blend mode), the same frames are opaque.
    let (mut p, cid) = (p, cid);
    p.comp_mut(cid).unwrap().layers[0].blend_mode = BlendMode::Screen;
    let (o, report, _) = export(&p, cid, 2);
    assert_eq!(report.layered, 0);
    assert_eq!(o.manifest["opaque"], true);
    let (o, _, _) = export(&p, cid, 1);
    assert_eq!(o.manifest["opaque"], true);
}

/// A precomp shrunk below half size is drawn from the compositor's box-filtered level; sampled
/// from the full-size pixels its fine detail would alias and the frames would fall back to
/// pictures.
#[test]
fn layers_shrunk_below_half_size_stay_layered() {
    use effectcraft_project::build::Ids;
    use effectcraft_project::{Node, PropGroup};
    let mut p = Project::default();
    let pre = p.add_item("Stripes", Label::Sandstone, None, ItemKind::Comp(Comp::new(400, 400, FrameRate::FPS_30, s(1.0)).into()));
    let pc = p.comp(pre).unwrap().clone();
    let mut shape = build::layer(&mut p, &pc, "Stripes", LayerSource::Shape, (400, 400), None);
    let mut next = p.next_id;
    let groups: Vec<PropGroup> = {
        let mut ids = Ids(&mut next);
        (0..40)
            .map(|i| {
                let rect = build::shape_rect(&mut ids, [2.0, 380.0], [10.0 * i as f64 + 5.0 - 200.0, 0.0], 0.0);
                let fill = build::shape_fill(&mut ids, [1.0, 0.9, 0.2, 1.0]);
                build::shape_group(&mut ids, "Stripe", vec![rect, fill])
            })
            .collect()
    };
    p.next_id = next;
    shape.props.sub_mut("contents").unwrap().children.extend(groups.into_iter().map(Node::Group));
    p.comp_mut(pre).unwrap().layers.push(shape);
    let main = p.add_item("Main", Label::Sandstone, None, ItemKind::Comp(Comp::new(160, 160, FrameRate::FPS_30, s(1.0)).into()));
    let mc = p.comp(main).unwrap().clone();
    let mut layer = build::layer(&mut p, &mc, "Stripes", LayerSource::Comp { item: pre }, (400, 400), None);
    animate(&mut layer, "scale", Value::Vec3([30.0, 30.0, 100.0]), Value::Vec3([30.0, 30.0, 100.0]));
    animate(&mut layer, "rotation", Value::Scalar(0.0), Value::Scalar(40.0));
    p.comp_mut(main).unwrap().layers.push(layer);
    let (o, report, rendered) = export(&p, main, 2);
    assert_eq!(report.layered, 30, "{report:?}");
    let Content::V2 { sprites, .. } = &o.content else { panic!("v2") };
    assert_eq!(sprites.len(), 1, "the shrunk precomp is stored once");
    assert!(sprites[0][3] <= 210, "the sprite is the box-filtered level (about 200 pixels), not the 400-pixel precomp");
    for (f, want) in rendered.iter().enumerate() {
        let (worst, share) = diff(&compose::frame(&o, f).to_rgba8(), want);
        assert!(worst <= 40 && share <= 0.002, "frame {f}: worst {worst}, {share}");
    }
}

/// A draw with a singular matrix (a layer scaled to nothing) or no opacity draws nothing; kept in
/// the file it made players invert a singular matrix (the reference player turned the whole frame
/// to NaN, so it showed empty).
#[test]
fn draws_that_cover_nothing_are_left_out() {
    let mut a = v2_animation();
    if let Frames::Layers(f) = &mut a.frames {
        f[0].push(Draw { sprite: 1, matrix: [581.0, -8.0, 0.0, 0.0, -500.0, 291.0], alpha: 1.0 });
        f[0].push(Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, 5.0, 5.0], alpha: 0.0007 });
    }
    let w = write(&a, &opts(false)).unwrap();
    let o = open(&w.file, &key_bytes()).unwrap();
    let Content::V2 { frames, .. } = &o.content else { panic!("v2") };
    assert_eq!(frames[0].len(), 1);

    // A layer growing from scale 0 never exports a singular draw.
    let (p, cid) = comp_with(|p, c| {
        let mut l = solid(p, c, [0.9, 0.5, 0.1], 30, 30);
        animate(&mut l, "scale", Value::Vec3([0.0, 0.0, 100.0]), Value::Vec3([100.0, 0.0, 100.0]));
        let mut k = solid(p, c, [0.1, 0.5, 0.9], 20, 20);
        animate(&mut k, "opacity", Value::Scalar(0.0), Value::Scalar(100.0));
        vec![l, k]
    });
    let (o, report, _) = export(&p, cid, 2);
    assert_eq!(report.layered, 30, "{report:?}");
    let Content::V2 { frames, .. } = &o.content else { panic!("v2") };
    for d in frames.iter().flatten() {
        let [a, b, c, dd, _, _] = d.matrix;
        assert!((a * dd - b * c).abs() > 1e-4 && d.alpha > 0.0, "{d:?}");
    }
}

// ---- Version 3: animations ----

use crate::container::{OpenedFile, Scene, decode_scene};
use crate::plan::{Range, build_animations};
use crate::{AnimationsOptions, AtlasFormat, Clip, Opened, write_animations};

fn v3_opts(format: AtlasFormat) -> AnimationsOptions {
    AnimationsOptions { key: Key::custom(KEY_HEX).unwrap(), atlas_size: None, atlas_format: format, nonces: Nonces { salt: [7; 16], nonce_prefix: [9; 8] } }
}

fn clip(key: &str, looped: bool, frames: usize) -> Clip {
    Clip { key: key.into(), looped, frames }
}

fn v3(o: &Opened) -> (&Vec<[u32; 5]>, &Scene) {
    let Content::V3 { sprites, scene } = &o.content else { panic!("v3") };
    (sprites, scene)
}

fn write_v3(a: &Animation, clips: &[Clip]) -> Opened {
    open(&write_animations(a, clips, &v3_opts(AtlasFormat::Png)).unwrap().file, &key_bytes()).unwrap()
}

/// Every plaintext data block of a version 3 file (atlases, then the deflated scene).
fn blocks(file: &[u8]) -> (serde_json::Value, Vec<Vec<u8>>) {
    let f = OpenedFile::open(file, &key_bytes()).unwrap();
    let m = f.manifest.clone();
    let span = |v: &serde_json::Value| (v["offset"].as_u64().unwrap(), v["length"].as_u64().unwrap());
    let mut out = vec![];
    for (i, a) in m["atlases"].as_array().unwrap().iter().enumerate() {
        let (o, l) = span(a);
        out.push(f.block(file, i as u32 + 1, o, l).unwrap());
    }
    let (o, l) = span(&m["scene"]);
    out.push(f.block(file, out.len() as u32 + 1, o, l).unwrap());
    (m, out)
}

/// The same file with its manifest changed.
fn with_manifest(file: &[u8], edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let (mut m, blocks) = blocks(file);
    edit(&mut m);
    crate::container::write_file(3, &Key::custom(KEY_HEX).unwrap(), &v3_opts(AtlasFormat::Png).nonces, &blocks, |_| m).unwrap().0
}

/// The same file with its scene replaced by `scene` (inflated bytes).
fn with_scene(file: &[u8], scene: &[u8]) -> Vec<u8> {
    let (m, mut blocks) = blocks(file);
    *blocks.last_mut().unwrap() = crate::container::deflate(scene);
    // The scene is the last block, so only its length changes.
    let sealed = blocks.last().unwrap().len() as u64 + 16;
    crate::container::write_file(3, &Key::custom(KEY_HEX).unwrap(), &v3_opts(AtlasFormat::Png).nonces, &blocks, |_| {
        let mut m = m;
        m["scene"]["length"] = sealed.into();
        m
    })
    .unwrap()
    .0
}

fn inflated_scene(file: &[u8]) -> Vec<u8> {
    miniz_oxide::inflate::decompress_to_vec(blocks(file).1.last().unwrap()).unwrap()
}

#[test]
fn version_3_round_trips() {
    let a = v2_animation();
    let w = write_animations(&a, &[clip("transition_in", false, 1), clip("idle", true, 2)], &v3_opts(AtlasFormat::Png)).unwrap();
    let h = read_header(&w.file).unwrap();
    assert_eq!((h.version, h.custom_key), (3, true));
    let o = open(&w.file, &key_bytes()).unwrap();
    let (sprites, scene) = v3(&o);
    // The red square is drawn by both animations, the green bar by `idle` only: an atlas each,
    // so a player that only plays `transition_in` holds one texture. Sprites start at the edge.
    assert_eq!(sprites, &vec![[0, 0, 0, 14, 14], [1, 0, 0, 22, 8]]);
    let m = &o.manifest;
    assert_eq!(m["version"], 3);
    assert_eq!(m["frameCount"], 3);
    assert_eq!(m["opaque"], false);
    assert_eq!(m["gpuBytes"], (14 * 14 + 22 * 8) * 4);
    assert_eq!(w.gpu_bytes, (14 * 14 + 22 * 8) * 4);
    assert_eq!(m["atlases"][0]["format"], "png");
    assert_eq!(
        m["animations"],
        serde_json::json!([
            {"key": "transition_in", "frameCount": 1, "loop": false, "atlases": [0]},
            {"key": "idle", "frameCount": 2, "loop": true, "atlases": [0, 1]},
        ])
    );
    assert_eq!((m["scene"]["draws"].as_u64(), m["scene"]["lists"].as_u64(), m["scene"]["items"].as_u64()), (Some(3), Some(3), Some(3)));
    assert_eq!(o.animations.len(), 2);
    let idle = o.animation("idle").unwrap();
    assert_eq!((idle.start, idle.frame_count, idle.looped, idle.atlases.clone()), (1, 2, true, vec![0, 1]));
    let Frames::Layers(frames) = &a.frames else { panic!("layers") };
    assert_eq!(scene.draws, frames.iter().flatten().copied().collect::<Vec<_>>());
    assert_eq!(scene.lists, vec![vec![0], vec![1, 2], vec![]]);
    assert_eq!(scene.frames, vec![0, 1, 2]);
    // The same pixels as the version 2 file of the same frames.
    let v2 = open(&write(&a, &opts(false)).unwrap().file, &key_bytes()).unwrap();
    for f in 0..3 {
        assert_eq!(compose::frame(&o, f).to_rgba8(), compose::frame(&v2, f).to_rgba8(), "frame {f}");
    }
    assert_eq!((w.frame_count, w.sprites, w.atlases, w.draws), (3, 2, 2, 3));
}

#[test]
fn version_3_deflates_the_manifest_and_the_scene() {
    let w = write_animations(&v2_animation(), &[Clip::main(3)], &v3_opts(AtlasFormat::Png)).unwrap();
    // Block 0 is a raw DEFLATE stream (no zlib header) of the manifest's JSON.
    let stored = crate::container::stored_manifest(&w.file, &key_bytes()).unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(&stored).is_err(), "not plain JSON");
    let json: serde_json::Value = serde_json::from_slice(&miniz_oxide::inflate::decompress_to_vec(&stored).unwrap()).unwrap();
    assert_eq!(json, w.manifest);
    assert_eq!(json["animations"][0], serde_json::json!({"key": "main", "frameCount": 3, "loop": true, "atlases": [0]}));
    // The scene: one column per draw field, list starts, items, frames.
    let scene = inflated_scene(&w.file);
    let (d, l, i, f) = (3, 3, 3, 3);
    assert_eq!(scene.len(), d * 32 + (l + 1) * 4 + i * 4 + f * 4);
    let u = |o: usize| u32::from_le_bytes(scene[o..o + 4].try_into().unwrap());
    let fl = |o: usize| f32::from_le_bytes(scene[o..o + 4].try_into().unwrap());
    assert_eq!([u(0), u(4), u(8)], [0, 0, 1], "sprite[D]");
    assert_eq!([fl(12), fl(16), fl(20)], [1.0, 0.866, 1.0], "a[D]");
    assert_eq!([fl(5 * 12), fl(5 * 12 + 4), fl(5 * 12 + 8)], [3.0, 13.4, 9.0], "tx[D]");
    assert_eq!([fl(7 * 12), fl(7 * 12 + 4), fl(7 * 12 + 8)], [1.0, 1.0, 0.5], "alpha[D]");
    assert_eq!((0..4).map(|k| u(96 + k * 4)).collect::<Vec<_>>(), vec![0, 1, 3, 3], "listStart[L + 1]");
    assert_eq!((0..3).map(|k| u(112 + k * 4)).collect::<Vec<_>>(), vec![0, 1, 2], "items[I]");
    assert_eq!((0..3).map(|k| u(124 + k * 4)).collect::<Vec<_>>(), vec![0, 1, 2], "frames[F]");
    // Atlases are not compressed again: block 1 is the PNG itself.
    assert_eq!(&blocks(&w.file).1[0][..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn every_different_draw_and_frame_is_stored_once() {
    let d0 = Draw { sprite: 0, matrix: [1.0, 0.0, 0.0, 1.0, 3.0, 3.0], alpha: 1.0 };
    // The same draw: -0 is 0.
    let d0_again = Draw { sprite: 0, matrix: [1.0, -0.0, 0.0, 1.0, 3.0, 3.0], alpha: 1.0 };
    let d1 = Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, 9.0, 33.0], alpha: 0.5 };
    let mut a = v2_animation();
    a.frames = Frames::Layers(vec![vec![d0], vec![d0_again, d1], vec![d0], vec![], vec![d1, d0], vec![], vec![d0, d1]]);
    let o = write_v3(&a, &[clip("a", false, 3), clip("b", true, 4)]);
    let (_, scene) = v3(&o);
    assert_eq!(scene.draws, vec![d0, d1]);
    // The order of draws is part of a frame: [d1, d0] is not [d0, d1].
    assert_eq!(scene.lists, vec![vec![0], vec![0, 1], vec![], vec![1, 0]]);
    assert_eq!(scene.frames, vec![0, 1, 0, 2, 3, 2, 1]);
    let bits = |d: &Draw| (d.sprite, d.matrix.map(f32::to_bits), d.alpha.to_bits());
    for (i, x) in scene.draws.iter().enumerate() {
        assert!(scene.draws[..i].iter().all(|y| bits(x) != bits(y)));
    }
    for (i, x) in scene.lists.iter().enumerate() {
        assert!(!scene.lists[..i].contains(x));
    }
}

#[test]
fn a_still_layer_costs_one_draw_and_one_list() {
    let mut a = v2_animation();
    let d = Draw { sprite: 0, matrix: [1.0, 0.0, 0.0, 1.0, 3.0, 3.0], alpha: 1.0 };
    a.frames = Frames::Layers(vec![vec![d]; 30]);
    let w = write_animations(&a, &[Clip::main(30)], &v3_opts(AtlasFormat::Png)).unwrap();
    let o = open(&w.file, &key_bytes()).unwrap();
    let (sprites, scene) = v3(&o);
    assert_eq!((sprites.len(), scene.draws.len(), scene.lists.len()), (1, 1, 1));
    assert_eq!(scene.frames, vec![0; 30]);
    // 32 bytes for the draw, 4 per frame.
    assert_eq!(inflated_scene(&w.file).len(), 32 + 2 * 4 + 4 + 30 * 4);

    // The same from a composition: a solid that never changes.
    let (p, cid) = comp_with(|p, c| vec![solid(p, c, [0.2, 0.4, 0.9], 30, 30)]);
    let r = Renderer::new(&p, &NoFootage, RenderOpts { backend: Backend::Cpu, ..Default::default() });
    let settings = Settings { version: 2, every_nth: 1, start: Tick(0), end: p.comp(cid).unwrap().duration, tolerance: Tolerance::default() };
    let (anim, clips, report) = build_animations(&r, cid, &settings, &[], &mut |_, _| true).unwrap();
    assert_eq!(clips, vec![Clip::main(30)], "without ranges: one animation, `main`, repeating");
    assert_eq!(report.layered, 30);
    let o = write_v3(&anim, &clips);
    let (sprites, scene) = v3(&o);
    assert_eq!((sprites.len(), scene.draws.len(), scene.lists.len()), (1, 1, 1));
    assert_eq!(scene.frames, vec![0; 30]);
}

#[test]
fn a_picture_two_animations_show_is_stored_once() {
    let a = Animation {
        width: 64,
        height: 48,
        fps: 12.0,
        composition_id: None,
        opaque: false,
        // Bitmap 2 has the pixels of bitmap 0.
        bitmaps: vec![
            bordered(12, 12, [200, 10, 10, 255]),
            bordered(20, 6, [10, 200, 10, 255]),
            bordered(12, 12, [200, 10, 10, 255]),
            bordered(4, 4, [9, 9, 9, 255]),
        ],
        frames: Frames::Layers(vec![
            vec![
                Draw { sprite: 0, matrix: [1.0, 0.0, 0.0, 1.0, 3.0, 3.0], alpha: 1.0 },
                Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, 9.0, 33.0], alpha: 1.0 },
            ],
            vec![Draw { sprite: 2, matrix: [1.0, 0.0, 0.0, 1.0, 3.0, 3.0], alpha: 1.0 }],
            vec![Draw { sprite: 2, matrix: [1.0, 0.0, 0.0, 1.0, 5.0, 3.0], alpha: 1.0 }],
        ]),
    };
    let o = write_v3(&a, &[clip("in", false, 1), clip("idle", true, 2)]);
    let (sprites, scene) = v3(&o);
    assert_eq!(sprites.len(), 2, "the square once, the bar once; the unused bitmap is dropped");
    assert_eq!(scene.draws.iter().map(|d| d.sprite).collect::<Vec<_>>(), vec![0, 1, 0]);
    assert_eq!(scene.lists, vec![vec![0, 1], vec![0], vec![2]], "frame 1 is the first draw of frame 0 again");
    // The square (both animations) and the bar (`in` only) are on an atlas each.
    assert_eq!(o.atlases.len(), 2);
    assert_eq!(o.manifest["animations"][0]["atlases"], serde_json::json!([0, 1]));
    assert_eq!(o.manifest["animations"][1]["atlases"], serde_json::json!([0]));
    assert_eq!(sprites, &vec![[0, 0, 0, 14, 14], [1, 0, 0, 22, 8]]);
}

#[test]
fn version_3_stores_a_picture_per_frame_as_one_draw() {
    let a = v1_animation();
    let o = write_v3(&a, &[Clip::main(4)]);
    let (sprites, scene) = v3(&o);
    // Each picture gets its 1-pixel border; the draw moves it back by that pixel.
    assert_eq!(sprites, &vec![[0, 0, 12, 7, 7], [0, 0, 0, 12, 12]]);
    assert_eq!(
        scene.draws,
        vec![Draw { sprite: 0, matrix: [1.0, 0.0, 0.0, 1.0, 3.0, 4.0], alpha: 1.0 }, Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, -1.0, -1.0], alpha: 1.0 }]
    );
    assert_eq!(scene.lists, vec![vec![0], vec![1], vec![]]);
    assert_eq!(scene.frames, vec![0, 1, 2, 0]);
    assert_eq!((o.atlases[0].width, o.atlases[0].height), (12, 19));
    let v1 = open(&write(&a, &opts(false)).unwrap().file, &key_bytes()).unwrap();
    for f in 0..4 {
        assert_eq!(compose::frame(&o, f).to_rgba8(), compose::frame(&v1, f).to_rgba8(), "frame {f}");
    }
}

#[test]
fn version_3_is_opaque_only_when_every_frame_is_one_opaque_picture_over_the_canvas() {
    let full = |frames: Frames, bitmaps: Vec<Bitmap>| Animation { width: 8, height: 6, fps: 10.0, composition_id: None, opaque: true, bitmaps, frames };
    let opaque = |a: &Animation| write_v3(a, &[Clip::main(a.frame_count())]).manifest["opaque"].clone();
    let at = |dx, dy| Some(Placed { bitmap: 0, dx, dy });
    let pictures = |f: Vec<Option<Placed>>| full(Frames::Sprites(f), vec![square(8, 6, [5, 6, 7, 255])]);
    assert_eq!(opaque(&pictures(vec![at(0, 0), at(0, 0)])), true);
    assert_eq!(opaque(&pictures(vec![at(0, 0), None])), false, "an empty frame");
    assert_eq!(opaque(&pictures(vec![at(1, 0)])), false, "not over the whole canvas");
    assert_eq!(opaque(&full(Frames::Sprites(vec![at(0, 0)]), vec![square(8, 6, [5, 6, 7, 254])])), false, "not opaque pixels");
    let layer =
        |m: [f32; 6], alpha: f32, n: usize| full(Frames::Layers(vec![vec![Draw { sprite: 0, matrix: m, alpha }; n]]), vec![bordered(8, 6, [5, 6, 7, 255])]);
    assert_eq!(opaque(&layer([1.0, 0.0, 0.0, 1.0, -1.0, -1.0], 1.0, 1)), true);
    assert_eq!(opaque(&layer([1.0, 0.0, 0.0, 1.0, -1.0, -1.0], 0.5, 1)), false, "faded");
    assert_eq!(opaque(&layer([2.0, 0.0, 0.0, 2.0, -2.0, -2.0], 1.0, 1)), false, "scaled");
    assert_eq!(opaque(&layer([1.0, 0.0, 0.0, 1.0, -1.0, -1.0], 1.0, 2)), false, "two draws blend");
}

#[test]
fn version_3_leaves_out_draws_that_change_no_pixel() {
    let mut a = v2_animation();
    let keep = Draw { sprite: 0, matrix: [1.0, 0.0, 0.0, 1.0, 3.0, 3.0], alpha: 1.0 };
    a.frames = Frames::Layers(vec![vec![
        keep,
        Draw { sprite: 1, matrix: [581.0, -8.0, 0.0, 0.0, -500.0, 291.0], alpha: 1.0 },
        Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, 5.0, 5.0], alpha: 0.0007 },
        // Right of the 64-pixel canvas, and above it.
        Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, 63.0, 5.0], alpha: 1.0 },
        Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, 5.0, -7.0], alpha: 1.0 },
        // One pixel column of the bar is still on the canvas.
        Draw { sprite: 1, matrix: [1.0, 0.0, 0.0, 1.0, 62.0, 5.0], alpha: 1.0 },
    ]]);
    let o = write_v3(&a, &[Clip::main(1)]);
    let (sprites, scene) = v3(&o);
    assert_eq!(scene.lists, vec![vec![0, 1]]);
    assert_eq!(scene.draws[1].matrix[4], 62.0);
    assert_eq!(sprites.len(), 2);
    // A sprite only such draws use is not stored.
    if let Frames::Layers(f) = &mut a.frames {
        f[0].pop();
    }
    assert_eq!(v3(&write_v3(&a, &[Clip::main(1)])).0.len(), 1);
    let mut bad = v2_animation();
    if let Frames::Layers(f) = &mut bad.frames {
        f[0][0].matrix[0] = f32::NAN;
    }
    assert!(write_animations(&bad, &[Clip::main(3)], &v3_opts(AtlasFormat::Png)).is_err());
}

#[test]
fn invalid_keys_and_clips_are_errors() {
    let a = v2_animation();
    let write = |clips: &[Clip]| write_animations(&a, clips, &v3_opts(AtlasFormat::Png)).map(|_| ()).map_err(|e| e.to_string());
    assert!(write(&[clip("transition_in", false, 1), clip("Idle-2.b", true, 2)]).is_ok());
    assert!(write(&[clip(&"k".repeat(64), true, 3)]).is_ok());
    for key in ["", " ", "two words", "é", "a/b", "a,b", &"k".repeat(65)] {
        let e = write(&[clip(key, true, 3)]).unwrap_err();
        assert!(e.contains("animation key is 1 to 64"), "{key:?}: {e}");
    }
    assert!(write(&[clip("a", true, 1), clip("a", true, 2)]).unwrap_err().contains("`a` is used twice"));
    assert!(write(&[]).unwrap_err().contains("at least one animation"));
    assert!(write(&[clip("a", true, 3), clip("b", true, 0)]).unwrap_err().contains("`b` has no frames"));
    assert!(write(&[clip("a", true, 2)]).unwrap_err().contains("take 2 frames, but 3"));
    assert!(write(&[clip("a", true, 2), clip("b", true, 2)]).unwrap_err().contains("take 4 frames, but 3"));
    let size = |s| write_animations(&a, &[Clip::main(3)], &AnimationsOptions { atlas_size: Some(s), ..v3_opts(AtlasFormat::Png) });
    assert!(size(0).is_err() && size(4097).is_err());
    assert!(matches!(size(16), Err(crate::Error::TooLarge(_))), "the 22-pixel bar does not fit");
    assert!(size(22).is_ok());
}

#[test]
fn invalid_ranges_are_errors_before_anything_is_rendered() {
    let (p, cid) = moving_logo();
    let r = Renderer::new(&p, &NoFootage, RenderOpts { backend: Backend::Cpu, ..Default::default() });
    let settings = |version| Settings { version, every_nth: 1, start: Tick(0), end: s(1.0), tolerance: Tolerance::default() };
    let range = |key: &str, first, last| Range { key: key.into(), looped: false, first, last };
    let mut rendered = 0;
    let mut build = |ranges: &[Range]| {
        build_animations(&r, cid, &settings(2), ranges, &mut |_, _| {
            rendered += 1;
            true
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
    };
    // The comp has frames 0 to 29.
    for (ranges, want) in [
        (vec![range("in", 0, 30)], "`in` is frames 0 to 30, which is not a range inside the composition (frames 0 to 29)"),
        (vec![range("in", -1, 5)], "`in` is frames -1 to 5"),
        (vec![range("in", 9, 8)], "`in` is frames 9 to 8"),
        (vec![range("in", 0, 3), range("idle", 4, i64::MAX)], "`idle` is frames 4 to"),
        (vec![range("not a key", 0, 3)], "animation key is 1 to 64"),
        (vec![range("in", 0, 3), range("in", 4, 5)], "`in` is used twice"),
    ] {
        let e = build(&ranges).unwrap_err();
        assert!(e.contains(want), "{e}");
    }
    assert_eq!(rendered, 0);
    assert!(build_animations(&r, cid, &settings(3), &[], &mut |_, _| true).is_err(), "frames are described as version 1 or 2");
    assert!(matches!(build_animations(&r, cid, &settings(2), &[range("in", 0, 3)], &mut |_, _| false), Err(crate::Error::Cancelled)));
}

#[test]
fn animations_are_frame_ranges_of_the_composition() {
    let (p, cid) = moving_logo();
    let cache = LayerCache::default();
    let mut r = Renderer::new(&p, &NoFootage, RenderOpts { backend: Backend::Cpu, ..Default::default() });
    r.cache = Some(&cache);
    let c = p.comp(cid).unwrap();
    let settings = |version, every_nth| Settings { version, every_nth, start: Tick(0), end: c.duration, tolerance: Tolerance::default() };
    let ranges = [
        Range { key: "transition_in".into(), looped: false, first: 0, last: 9 },
        // Shares frames 8 and 9 with `transition_in`, and ends on the comp's last frame.
        Range { key: "idle".into(), looped: true, first: 8, last: 29 },
        Range { key: "still".into(), looped: false, first: 9, last: 9 },
    ];
    for version in [1, 2] {
        let (anim, clips, report) = build_animations(&r, cid, &settings(version, 1), &ranges, &mut |_, _| true).unwrap();
        assert_eq!(clips, vec![clip("transition_in", false, 10), clip("idle", true, 22), clip("still", false, 1)]);
        assert_eq!((anim.frame_count(), report.frames), (33, 33));
        assert_eq!(report.layered, if version == 2 { 33 } else { 0 });
        let o = write_v3(&anim, &clips);
        let (sprites, scene) = v3(&o);
        assert_eq!(o.manifest["frameCount"], 33);
        // Frames two animations share are the same list.
        assert_eq!(scene.frames[8..10], scene.frames[10..12]);
        assert_eq!(scene.frames[32], scene.frames[9]);
        if version == 2 {
            assert_eq!(sprites.len(), 2, "two layers, each stored once for all animations");
            assert_eq!(o.atlases.len(), 1, "every animation draws both");
        }
        // Each animation's frames are the comp's.
        for a in &o.animations {
            let first = ranges.iter().find(|r| r.key == a.key).unwrap().first;
            for n in [0, a.frame_count - 1] {
                let want = r.comp_frame(cid, c.frame_rate.tick_of(first + n as i64)).to_rgba8();
                let (worst, share) = diff(&compose::frame(&o, a.start + n).to_rgba8(), &want);
                assert!(worst <= 40 && share <= 0.002, "{} frame {n}: worst {worst}, {share}", a.key);
            }
        }
    }
    // Every third frame of each range, from its first frame.
    let (anim, clips, _) = build_animations(&r, cid, &settings(2, 3), &ranges, &mut |_, _| true).unwrap();
    assert_eq!(clips.iter().map(|c| c.frames).collect::<Vec<_>>(), vec![4, 8, 1], "0 3 6 9, 8 11 … 29, 9");
    assert_eq!(anim.fps, 10.0);
}

#[test]
fn version_3_packs_sprites_edge_to_edge_on_the_smallest_page() {
    // Sorted by height, on the width that needs the fewest pixels: the bar goes below the square.
    let (pages, places) = crate::atlas::pack_sprites(&[(14, 14), (22, 8)], 2048).unwrap();
    assert_eq!(pages, vec![crate::atlas::Page { width: 22, height: 22 }]);
    assert_eq!(places.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(), vec![(0, 0), (0, 14)]);
    // The higher sprites come first whatever the order given: 20 × 34 pixels, not 30 × 30.
    let (pages, places) = crate::atlas::pack_sprites(&[(10, 4), (10, 30), (10, 30)], 2048).unwrap();
    assert_eq!(pages, vec![crate::atlas::Page { width: 20, height: 34 }]);
    assert_eq!(places.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>(), vec![(0, 30), (0, 0), (10, 0)]);
    // A full page is followed by the next one, in the order given.
    let (pages, places) = crate::atlas::pack_sprites(&[(60, 60); 5], 128).unwrap();
    assert_eq!(pages, vec![crate::atlas::Page { width: 120, height: 120 }, crate::atlas::Page { width: 60, height: 60 }]);
    assert_eq!(places.iter().map(|p| p.page).collect::<Vec<_>>(), vec![0, 0, 0, 0, 1]);
    assert!(matches!(crate::atlas::pack_sprites(&[(129, 4)], 128), Err(crate::Error::TooLarge(_))));
    assert!(crate::atlas::pack_sprites(&[(0, 4)], 128).is_err());
    assert_eq!(crate::atlas::pack_sprites(&[], 128).unwrap().0, vec![]);
    // No two sprites overlap, and every one is inside its page.
    let sizes: Vec<(u32, u32)> = (0..200u32).map(|i| (3 + i * 7 % 50, 3 + i * 13 % 40)).collect();
    let (pages, places) = crate::atlas::pack_sprites(&sizes, 256).unwrap();
    assert!(pages.len() > 1 && pages.iter().all(|p| p.width <= 256 && p.height <= 256));
    for (i, (a, sa)) in places.iter().zip(&sizes).enumerate() {
        assert!(a.x + sa.0 <= pages[a.page as usize].width && a.y + sa.1 <= pages[a.page as usize].height);
        for (b, sb) in places.iter().zip(&sizes).skip(i + 1) {
            let apart = a.page != b.page || a.x + sa.0 <= b.x || b.x + sb.0 <= a.x || a.y + sa.1 <= b.y || b.y + sb.1 <= a.y;
            assert!(apart, "{a:?} {sa:?} and {b:?} {sb:?}");
        }
    }
    assert!(places.windows(2).all(|w| w[0].page <= w[1].page), "pages are filled in the order given");
}

#[test]
fn many_small_groups_of_animations_share_atlases() {
    // Ten animations, each with a picture of its own: more groups than a player should hold
    // textures for, so the small ones are put together.
    let bitmaps: Vec<Bitmap> = (0..10u8).map(|i| bordered(4 + i as u32, 4, [i, 9, 9, 255])).collect();
    let frames = (0..10).map(|i| vec![Draw { sprite: i, matrix: [1.0, 0.0, 0.0, 1.0, 3.0, 3.0], alpha: 1.0 }]).collect();
    let a = Animation { width: 64, height: 48, fps: 12.0, composition_id: None, opaque: false, bitmaps, frames: Frames::Layers(frames) };
    let clips: Vec<Clip> = (0..10).map(|i| clip(&format!("a{i}"), false, 1)).collect();
    let o = write_v3(&a, &clips);
    assert_eq!(o.atlases.len(), 1);
    assert_eq!(v3(&o).0.len(), 10);
    // With eight or fewer, each keeps its own atlas.
    let a8 = Animation {
        frames: Frames::Layers(match &a.frames {
            Frames::Layers(f) => f[..8].to_vec(),
            Frames::Sprites(_) => panic!("layers"),
        }),
        ..a.clone()
    };
    let o = write_v3(&a8, &clips[..8]);
    assert_eq!(o.atlases.len(), 8);
    for (i, an) in o.animations.iter().enumerate() {
        assert_eq!(an.atlases, vec![i as u32]);
    }
}

#[test]
fn the_smallest_format_is_chosen_for_each_atlas() {
    let a = v2_animation();
    let clips = [clip("in", false, 1), clip("idle", true, 2)];
    let write = |f| write_animations(&a, &clips, &v3_opts(f)).unwrap();
    let (png, webp, smallest) = (write(AtlasFormat::Png), write(AtlasFormat::Webp), write(AtlasFormat::Smallest));
    let (pb, wb, sb) = (blocks(&png.file).1, blocks(&webp.file).1, blocks(&smallest.file).1);
    let n = smallest.manifest["atlases"].as_array().unwrap().len();
    assert!(n >= 2);
    for i in 0..n {
        // Never larger than either, and labelled as what it is.
        assert_eq!(sb[i].len(), pb[i].len().min(wb[i].len()));
        let format = smallest.manifest["atlases"][i]["format"].as_str().unwrap();
        assert_eq!(format, if &sb[i][..4] == b"RIFF" { "webp" } else { "png" });
    }
    let (p, s) = (open(&png.file, &key_bytes()).unwrap(), open(&smallest.file, &key_bytes()).unwrap());
    assert_eq!(p.atlases, s.atlases);
}

#[test]
fn version_3_atlases_can_be_webp() {
    let a = v2_animation();
    let clips = [clip("in", false, 1), clip("idle", true, 2)];
    let png = write_animations(&a, &clips, &v3_opts(AtlasFormat::Png)).unwrap();
    let webp = write_animations(&a, &clips, &v3_opts(AtlasFormat::Webp)).unwrap();
    assert_eq!(webp.manifest["atlases"][0]["format"], "webp");
    assert_eq!(webp.manifest["atlases"][1]["format"], "webp");
    assert_eq!(&blocks(&webp.file).1[0][..4], b"RIFF");
    let (p, w) = (open(&png.file, &key_bytes()).unwrap(), open(&webp.file, &key_bytes()).unwrap());
    // Lossless: the same pixels.
    assert_eq!(p.atlases, w.atlases);
    assert_eq!(p.content, w.content);
    for f in 0..3 {
        assert_eq!(compose::frame(&p, f).to_rgba8(), compose::frame(&w, f).to_rgba8());
    }
    // Translucent pixels survive too.
    let mut soft = v1_animation();
    soft.frames = Frames::Sprites(vec![Some(Placed { bitmap: 1, dx: 4, dy: 5 }), Some(Placed { bitmap: 2, dx: 0, dy: 0 })]);
    let o = open(&write_animations(&soft, &[Clip::main(2)], &v3_opts(AtlasFormat::Webp)).unwrap().file, &key_bytes()).unwrap();
    let f0 = compose::frame(&o, 0).to_rgba8();
    assert_eq!(&f0[((5 * 40 + 4) * 4) as usize..][..4], &[0, 0, 255, 128]);
    // A file that says PNG but holds WebP is refused, as is an unknown format.
    assert!(open(&with_manifest(&webp.file, |m| m["atlases"][0]["format"] = "png".into()), &key_bytes()).is_err());
    assert!(open(&with_manifest(&webp.file, |m| m["atlases"][0]["format"] = "gif".into()), &key_bytes()).is_err());
}

/// The spec's "Reject ..." rules for a version 3 manifest.
#[test]
fn a_bad_version_3_manifest_is_rejected() {
    let w = write_animations(&v2_animation(), &[clip("in", false, 1), clip("idle", true, 2)], &v3_opts(AtlasFormat::Png)).unwrap();
    assert!(open(&with_manifest(&w.file, |_| {}), &key_bytes()).is_ok(), "rewritten unchanged, it opens");
    let cases: Vec<(&str, Box<dyn Fn(&mut serde_json::Value)>)> = vec![
        ("version is not 3", Box::new(|m| m["version"] = 2.into())),
        ("keys are not unique", Box::new(|m| m["animations"][1]["key"] = "in".into())),
        ("a key does not match the pattern", Box::new(|m| m["animations"][1]["key"] = "id le".into())),
        ("a key is empty", Box::new(|m| m["animations"][1]["key"] = "".into())),
        ("a key is too long", Box::new(|m| m["animations"][1]["key"] = "k".repeat(65).into())),
        ("a key is not text", Box::new(|m| m["animations"][1]["key"] = 5.into())),
        ("a frame count is 0", Box::new(|m| m["animations"][0]["frameCount"] = 0.into())),
        ("a frame count is not a whole number", Box::new(|m| m["animations"][0]["frameCount"] = 1.5.into())),
        ("frame counts add up to less", Box::new(|m| m["animations"][1]["frameCount"] = 1.into())),
        ("frame counts add up to more", Box::new(|m| m["animations"][1]["frameCount"] = 3.into())),
        ("frameCount is another", Box::new(|m| m["frameCount"] = 4.into())),
        ("an animation names a missing atlas", Box::new(|m| m["animations"][0]["atlases"] = serde_json::json!([0, 2]))),
        ("atlases is not a list", Box::new(|m| m["animations"][0]["atlases"] = 0.into())),
        ("no animations", Box::new(|m| m["animations"] = serde_json::json!([]))),
        (
            "animations is missing",
            Box::new(|m| {
                m.as_object_mut().unwrap().remove("animations");
            }),
        ),
        (
            "the scene is missing",
            Box::new(|m| {
                m.as_object_mut().unwrap().remove("scene");
            }),
        ),
        ("the scene holds fewer draws than said", Box::new(|m| m["scene"]["draws"] = 4.into())),
        ("the scene holds more lists than said", Box::new(|m| m["scene"]["lists"] = 2.into())),
        ("the scene holds more items than said", Box::new(|m| m["scene"]["items"] = 2.into())),
        ("a sprite reaches outside its atlas", Box::new(|m| m["sprites"][0][3] = 15.into())),
        (
            "a draw's sprite is missing",
            Box::new(|m| {
                m["sprites"].as_array_mut().unwrap().pop();
            }),
        ),
    ];
    for (what, edit) in cases {
        assert!(open(&with_manifest(&w.file, edit), &key_bytes()).is_err(), "{what}");
    }
    // Unknown fields are ignored.
    assert!(open(&with_manifest(&w.file, |m| m["animations"][0]["later"] = true.into()), &key_bytes()).is_ok());
}

/// The spec's "Reject ..." rules for a scene.
#[test]
fn a_bad_scene_is_rejected() {
    let w = write_animations(&v2_animation(), &[clip("in", false, 1), clip("idle", true, 2)], &v3_opts(AtlasFormat::Png)).unwrap();
    let good = inflated_scene(&w.file);
    // D = 3, L = 3, I = 3, F = 3: listStart at 96, items at 112, frames at 124.
    assert_eq!(good.len(), 136);
    assert!(decode_scene(&good, 3, 3, 3, 3, 2).is_ok());
    assert!(open(&with_scene(&w.file, &good), &key_bytes()).is_ok(), "rewritten unchanged, it opens");
    let put = |at: usize, v: u32| {
        let mut s = good.clone();
        s[at..at + 4].copy_from_slice(&v.to_le_bytes());
        s
    };
    for (what, scene) in [
        ("a sprite index is not below sprites.length", put(8, 2)),
        ("listStart[0] is not 0", put(96, 1)),
        ("listStart decreases", put(100, 4)),
        ("listStart decreases at the end", put(104, 4)),
        ("listStart[L] is not I", put(108, 2)),
        ("listStart[L] is above I", put(108, 4)),
        ("an item is not below D", put(116, 3)),
        ("a frame is not below L", put(132, 3)),
        ("the stream holds less", good[..132].to_vec()),
        ("the stream holds more", [good.clone(), vec![0; 4]].concat()),
        ("the stream is empty", vec![]),
    ] {
        assert!(decode_scene(&scene, 3, 3, 3, 3, 2).is_err(), "{what}");
        assert!(open(&with_scene(&w.file, &scene), &key_bytes()).is_err(), "{what}, in a file");
    }
    // A scene that is not a DEFLATE stream.
    let (m, mut b) = blocks(&w.file);
    *b.last_mut().unwrap() = good.clone();
    let stored = crate::container::write_file(3, &Key::custom(KEY_HEX).unwrap(), &v3_opts(AtlasFormat::Png).nonces, &b, |spans| {
        let mut m = m;
        m["scene"]["length"] = spans.last().unwrap().1.into();
        m
    })
    .unwrap()
    .0;
    assert!(open(&stored, &key_bytes()).is_err());
    // Sizes that overflow are refused, not computed.
    assert!(decode_scene(&good, usize::MAX / 8, 3, 3, 3, 2).is_err());
    assert!(decode_scene(&good, 3, usize::MAX, usize::MAX, usize::MAX, 2).is_err());
}

#[test]
fn damaged_version_3_files_never_panic() {
    let w = write_animations(&v2_animation(), &[clip("in", false, 1), clip("idle", true, 2)], &v3_opts(AtlasFormat::Png)).unwrap();
    for n in 0..w.file.len() {
        assert!(open(&w.file[..n], &key_bytes()).is_err(), "cut at {n}");
    }
    for i in [9, 20, 45, w.file.len() - 1] {
        let mut f = w.file.clone();
        f[i] ^= 1;
        assert!(open(&f, &key_bytes()).is_err(), "byte {i}");
    }
    assert!(open(&w.file, &[1; 32]).is_err());
    // A manifest that is not deflated, and one that inflates to something else than JSON.
    let (m, b) = blocks(&w.file);
    let as_v2 = crate::container::write_file(2, &Key::custom(KEY_HEX).unwrap(), &v3_opts(AtlasFormat::Png).nonces, &b, |_| m).unwrap().0;
    let mut plain = as_v2.clone();
    plain[8] = 3;
    assert!(open(&plain, &key_bytes()).is_err());
    assert!(
        read_header(&{
            let mut f = w.file.clone();
            f[8] = 4;
            f
        })
        .is_err()
    );
}

/// The files version 1 and 2 exports wrote before version 3 existed, byte for byte: with the
/// salt and nonce fixed, everything but the PNG encoder's own choices is pinned here (the
/// manifest's text, the sealed sizes, the timeline's bytes).
#[test]
fn versions_1_and_2_are_written_as_before() {
    let w = write(&v1_animation(), &opts(false)).unwrap();
    let plain = crate::container::stored_manifest(&w.file, &key_bytes()).unwrap();
    assert_eq!(w.file[..16], [0x89, 0x57, 0x41, 0x4E, 0x49, 0x4D, 0x0D, 0x0A, 1, 0, 0, 0, (plain.len() + 16) as u8, ((plain.len() + 16) >> 8) as u8, 0, 0]);
    let length = w.manifest["atlases"][0]["length"].as_u64().unwrap();
    assert_eq!(
        String::from_utf8(plain.clone()).unwrap(),
        format!(
            r#"{{"atlases":[{{"format":"png","height":14,"length":{length},"offset":0,"width":21}}],"compositionId":"ball","fps":30.0,"frameCount":4,"frames":[0,1,-1,0],"gpuBytes":1176,"height":30,"opaque":false,"sprites":[[0,2,2,5,5,4,5],[0,9,2,10,10,0,0]],"version":1,"width":40}}"#
        )
    );
    assert_eq!(w.file.len() as u64, 40 + plain.len() as u64 + 16 + length);

    let w = write(&v2_animation(), &opts(true)).unwrap();
    let plain = crate::container::stored_manifest(&w.file, &key_bytes()).unwrap();
    assert_eq!(w.file[8..12], [2, 0, 1, 0]);
    let length = w.manifest["atlases"][0]["length"].as_u64().unwrap();
    assert_eq!(
        String::from_utf8(plain.clone()).unwrap(),
        format!(
            r#"{{"atlases":[{{"format":"png","height":18,"length":{length},"offset":0,"width":42}}],"compositionId":null,"fps":12.0,"frameCount":3,"gpuBytes":3024,"height":48,"opaque":false,"sprites":[[0,2,2,14,14],[0,18,2,22,8]],"timeline":{{"draws":3,"length":128,"offset":{length}}},"version":2,"width":64}}"#
        )
    );
    let f = OpenedFile::open(&w.file, &key_bytes()).unwrap();
    let timeline = f.block(&w.file, 2, length, 128).unwrap();
    assert_eq!(timeline.len(), 4 * 4 + 3 * 32);
    assert_eq!(timeline[..16], [0, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 3, 0, 0, 0]);
    let Frames::Layers(frames) = v2_animation().frames else { panic!("layers") };
    let mut draws = vec![];
    for d in frames.iter().flatten() {
        draws.extend(d.sprite.to_le_bytes());
        draws.extend(d.matrix.iter().flat_map(|v| v.to_le_bytes()));
        draws.extend(d.alpha.to_le_bytes());
    }
    assert_eq!(timeline[16..], draws, "32 bytes per draw, in frame order");
    assert_eq!(w.file.len() as u64, 40 + plain.len() as u64 + 16 + length + 128);
}

/// The format's version 3 fixtures, when a local copy is available (`EFFECTCRAFT_WANIM_FIXTURES`):
/// the reader reproduces the expected manifest, atlases, scene and frames, and the writer, given
/// the animations the fixture was packed from, writes the same sprites, scene and atlas pixels.
#[test]
fn reads_and_reproduces_the_version_3_fixtures() {
    let Ok(dir) = std::env::var("EFFECTCRAFT_WANIM_FIXTURES") else { return };
    let dir = std::path::Path::new(&dir);
    let exp: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("expected-animations.json")).unwrap()).unwrap();
    let key = crate::container::parse_hex_key(exp["customKey"].as_str().unwrap()).unwrap();
    let bytes = std::fs::read(dir.join("sample-animations-custom-key.wanim")).unwrap();
    let o = open(&bytes, &key).unwrap();
    assert_eq!(o.manifest, exp["manifest"]);
    let f = OpenedFile::open(&bytes, &key).unwrap();
    for (i, a) in exp["manifest"]["atlases"].as_array().unwrap().iter().enumerate() {
        let png = f.block(&bytes, i as u32 + 1, a["offset"].as_u64().unwrap(), a["length"].as_u64().unwrap()).unwrap();
        assert_eq!(format!("{:x}", sha2::Sha256::digest(&png)), exp["atlasSha256"][i].as_str().unwrap(), "atlas {i}");
    }
    let same_scene = |o: &Opened, what: &str| {
        let (_, scene) = v3(o);
        let want = exp["scene"]["draws"].as_array().unwrap();
        assert_eq!(scene.draws.len(), want.len(), "{what}");
        for (d, w) in scene.draws.iter().zip(want) {
            assert_eq!(d.sprite as u64, w["sprite"].as_u64().unwrap(), "{what}");
            for (a, b) in d.matrix.iter().zip(w["matrix"].as_array().unwrap()) {
                assert_eq!(*a as f64, b.as_f64().unwrap(), "{what}");
            }
            assert_eq!(d.alpha as f64, w["alpha"].as_f64().unwrap(), "{what}");
        }
        assert_eq!(serde_json::json!(scene.lists), exp["scene"]["lists"], "{what}");
        for a in &o.animations {
            assert_eq!(serde_json::json!(scene.frames[a.start..a.start + a.frame_count]), exp["scene"]["frames"][&a.key], "{what} {}", a.key);
        }
    };
    same_scene(&o, "fixture");
    // Atlas pixels as the fixture hashes them: straight alpha, invisible pixels set to 0.
    let pixels = |o: &Opened, i: usize| {
        let mut px = o.atlases[i].rgba.clone();
        for p in px.as_chunks_mut::<4>().0 {
            if p[3] == 0 {
                *p = [0; 4];
            }
        }
        format!("{:x}", sha2::Sha256::digest(&px))
    };
    let same_frames = |o: &Opened, what: &str| {
        for a in &o.animations {
            for n in 0..a.frame_count {
                let png = std::fs::read(dir.join(format!("animations-{}-{n}.png", a.key))).unwrap();
                let want = crate::atlas::decode_png(&png).unwrap();
                let (worst, _) = diff(&compose::frame(o, a.start + n).to_rgba8(), &want.rgba);
                assert!(worst <= 1, "{what} {} frame {n}: worst {worst}", a.key);
            }
        }
    };
    same_frames(&o, "fixture");
    for i in 0..o.atlases.len() {
        assert_eq!(pixels(&o, i), exp["atlasPixelsSha256"][i].as_str().unwrap(), "atlas {i}");
    }

    // The animations the fixture was packed from, written here.
    let turn = |degrees: f64, x: f64, y: f64, size: f64| {
        let (s, c) = (degrees * std::f64::consts::PI / 180.0).sin_cos();
        [c, s, -s, c, x - (c * size - s * size) / 2.0, y - (s * size + c * size) / 2.0]
    };
    let at = |x: f64, y: f64| [1.0, 0.0, 0.0, 1.0, x, y];
    // The fixture's matrices place the picture; here they place the sprite, border included.
    let d = |bitmap: u32, m: [f64; 6], alpha: f32| {
        let [a, b, c, dd, tx, ty] = m;
        Draw { sprite: bitmap, matrix: [a, b, c, dd, -a - c + tx, -b - dd + ty].map(|v| v as f32), alpha }
    };
    let paint = |w: u32, h: u32, f: &dyn Fn(u32, u32) -> [u8; 4]| {
        let mut b = square(w + 2, h + 2, [0; 4]);
        for y in 0..h {
            for x in 0..w {
                let i = (((y + 1) * (w + 2) + x + 1) * 4) as usize;
                b.rgba[i..i + 4].copy_from_slice(&f(x, y));
            }
        }
        b
    };
    let star = |x: u32, y: u32| if x.abs_diff(2) + y.abs_diff(2) <= 2 { [255, 230, 80, 255] } else { [0; 4] };
    let (badge, bar) = (d(0, at(26.0, 18.0), 1.0), d(1, at(22.0, 36.0), 1.0));
    let anim = Animation {
        width: 64,
        height: 48,
        fps: 12.0,
        composition_id: Some("sample-animations".into()),
        opaque: false,
        bitmaps: vec![
            paint(12, 12, &|x, y| [255, (x * 20) as u8, (y * 20) as u8, 255]),
            paint(20, 6, &|_, _| [40, 120, 255, 160]),
            paint(5, 5, &star),
            paint(5, 5, &star),
        ],
        frames: Frames::Layers(vec![
            vec![],
            vec![d(0, turn(60.0, 8.0, 24.0, 12.0), 0.5)],
            vec![d(0, turn(30.0, 20.0, 24.0, 12.0), 0.75), d(1, [0.5, 0.0, 0.0, 1.0, 27.0, 36.0], 0.5)],
            vec![badge, bar],
            vec![badge, bar],
            vec![badge, bar, d(2, at(44.0, 8.0), 1.0)],
            vec![badge, bar, d(2, at(44.0, 8.0), 1.0)],
            vec![badge, bar, d(3, [2.0, 0.0, 0.0, 2.0, 40.0, 4.0], 0.5), d(3, at(12.0, 8.0), 1.0)],
            vec![badge, bar],
            vec![d(0, at(26.0, 18.0), 0.5), d(1, [1.5, 0.0, 0.0, 1.0, 17.0, 36.0], 0.5)],
            vec![d(0, [-1.0, 0.0, 0.0, 1.0, 70.0, 40.0], 0.25)],
        ]),
    };
    let clips = [clip("transition_in", false, 4), clip("idle", true, 4), clip("transition_out", false, 3)];
    for format in [AtlasFormat::Png, AtlasFormat::Webp] {
        let w =
            write_animations(&anim, &clips, &AnimationsOptions { key: Key::custom(exp["customKey"].as_str().unwrap()).unwrap(), ..v3_opts(format) }).unwrap();
        let mine = open(&w.file, &key).unwrap();
        let want = &exp["manifest"];
        for k in ["version", "compositionId", "width", "height", "frameCount", "opaque", "gpuBytes", "sprites", "animations"] {
            assert_eq!(mine.manifest[k], want[k], "{k}");
        }
        assert_eq!(mine.manifest["fps"].as_f64(), want["fps"].as_f64());
        for k in ["draws", "lists", "items"] {
            assert_eq!(mine.manifest["scene"][k], want["scene"][k], "scene.{k}");
        }
        for (i, a) in want["atlases"].as_array().unwrap().iter().enumerate() {
            assert_eq!((&mine.manifest["atlases"][i]["width"], &mine.manifest["atlases"][i]["height"]), (&a["width"], &a["height"]), "atlas {i}");
            assert_eq!(mine.manifest["atlases"][i]["format"], format.name());
            assert_eq!(pixels(&mine, i), exp["atlasPixelsSha256"][i].as_str().unwrap(), "atlas {i} pixels, {format:?}");
        }
        same_scene(&mine, "written here");
        same_frames(&mine, "written here");
    }

    // The fixtures with the official key, when it is given.
    let Ok(hex) = std::env::var("EFFECTCRAFT_WANIM_OFFICIAL_KEY") else { return };
    let official = crate::container::parse_hex_key(&hex).unwrap();
    let o = open(&std::fs::read(dir.join("sample-animations.wanim")).unwrap(), &official).unwrap();
    assert_eq!(o.manifest, exp["manifest"]);
    let o = open(&std::fs::read(dir.join("sample-animations-webp.wanim")).unwrap(), &official).unwrap();
    assert_eq!(o.manifest, exp["webp"]["manifest"]);
    same_scene(&o, "WebP fixture");
    same_frames(&o, "WebP fixture");
    for i in 0..o.atlases.len() {
        assert_eq!(pixels(&o, i), exp["webp"]["atlasPixelsSha256"][i].as_str().unwrap(), "WebP atlas {i}");
    }
}

/// Writes the moving logo as version 3 files for the format's own reader to check, with what was
/// written beside them as JSON, when the key and an output folder are given
/// (`EFFECTCRAFT_WANIM_OFFICIAL_KEY`, `EFFECTCRAFT_WANIM_OUT`). The key is never part of the
/// repository.
#[test]
fn writes_version_3_files_for_the_formats_reader() {
    let (Ok(hex), Ok(out)) = (std::env::var("EFFECTCRAFT_WANIM_OFFICIAL_KEY"), std::env::var("EFFECTCRAFT_WANIM_OUT")) else { return };
    let out = std::path::Path::new(&out);
    let official = crate::container::parse_hex_key(&hex).unwrap();
    let (p, cid) = moving_logo();
    let mut r = Renderer::new(&p, &NoFootage, RenderOpts { backend: Backend::Cpu, ..Default::default() });
    let cache = LayerCache::default();
    r.cache = Some(&cache);
    let c = p.comp(cid).unwrap();
    let ranges = [
        Range { key: "transition_in".into(), looped: false, first: 0, last: 9 },
        Range { key: "idle".into(), looped: true, first: 10, last: 19 },
        Range { key: "transition_out".into(), looped: false, first: 15, last: 29 },
    ];
    for (name, version, ranges, format) in [
        ("logo-v3", 2, &ranges[..], AtlasFormat::Png),
        ("logo-v3-webp", 2, &ranges[..], AtlasFormat::Webp),
        ("logo-v3-pictures", 1, &ranges[..], AtlasFormat::Png),
        ("logo-v3-main", 2, &[][..], AtlasFormat::Png),
    ] {
        let settings = Settings { version, every_nth: 1, start: Tick(0), end: c.duration, tolerance: Tolerance::default() };
        let (anim, clips, _) = build_animations(&r, cid, &settings, ranges, &mut |_, _| true).unwrap();
        let opts = AnimationsOptions { key: Key::official(&hex).unwrap(), atlas_size: None, atlas_format: format, nonces: Nonces::random().unwrap() };
        let w = write_animations(&anim, &clips, &opts).unwrap();
        std::fs::write(out.join(format!("{name}.wanim")), &w.file).unwrap();
        let o = open(&w.file, &official).unwrap();
        let (_, scene) = v3(&o);
        let f = OpenedFile::open(&w.file, &official).unwrap();
        let atlas_sha: Vec<String> = w.manifest["atlases"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, a)| {
                format!("{:x}", sha2::Sha256::digest(f.block(&w.file, i as u32 + 1, a["offset"].as_u64().unwrap(), a["length"].as_u64().unwrap()).unwrap()))
            })
            .collect();
        let draws: Vec<serde_json::Value> = scene.draws.iter().map(|d| serde_json::json!({"sprite": d.sprite, "matrix": d.matrix, "alpha": d.alpha})).collect();
        let frames: serde_json::Map<String, serde_json::Value> =
            o.animations.iter().map(|a| (a.key.clone(), serde_json::json!(scene.frames[a.start..a.start + a.frame_count]))).collect();
        let written = serde_json::json!({"manifest": w.manifest, "atlasSha256": atlas_sha, "scene": {"draws": draws, "lists": scene.lists, "frames": frames}});
        std::fs::write(out.join(format!("{name}.json")), serde_json::to_vec_pretty(&written).unwrap()).unwrap();
    }
}
