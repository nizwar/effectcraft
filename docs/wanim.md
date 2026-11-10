# `.wanim` export

File ▸ Export ▸ WANIM… (`file.exportWanim`) writes the active comp as a `.wanim` sprite
animation: texture atlases plus a manifest of what to draw in each frame, encrypted with
AES-256-GCM so that only players holding the key can read it. The crate is
`crates/wanim` (`effectcraft-wanim`).

The menu entry opens a dialog with the file, the version, whether frames are layers, where the
animations come from (one animation, or the comp markers) and the atlas format. The other
parameters are for scripts and agents.

| Version | What the file stores | Use it for |
|---|---|---|
| 3 (default) | One or more animations with keys. A frame is a list of draws (a sprite, a matrix, an opacity); every different draw and every different frame is stored once | New exports: an intro, a loop and an outro in one file, and the smallest files |
| 2 | One animation. A frame is a list of sprites, each drawn with a matrix and an opacity | Players that read version 2 only |
| 1 | One animation. A frame is one sprite: the frame cropped to its pixels | Players that read version 1 only |

Version 1 and 2 files are written exactly as before version 3 existed.

## Version 3

A version 3 file is smaller than the same frames in version 2, and it can hold several animations:

- A layer that stands still costs one draw and one list however long it is shown, and a held
  frame costs 4 bytes.
- The manifest and the scene (the draws, the lists and the list of each frame) are deflated
  before they are encrypted.
- Sprites touch each other on the atlas (each has its own 1-pixel transparent border), and each
  atlas is as small as its sprites allow.
- Atlases are PNG or lossless WebP (`atlasFormat`). By default each atlas is stored in whichever of the two is fewer bytes: this encoder's WebP is smaller for most pictures and larger for some.

Frames are described as layers, as in version 2, or with `layers: false` as one picture each, as
in version 1 (footage-like content where every frame looks different anyway).

### Animations

Without `animations` the file has one animation with the key `main`, which repeats, over the
whole comp (or the work area). To put several animations in one file, name them:

```json
{"cmd": "file.exportWanim", "params": {"path": "badge.wanim", "animations": [
  {"key": "transition_in", "firstFrame": 0, "lastFrame": 29},
  {"key": "idle", "firstFrame": 30, "lastFrame": 89, "loop": true},
  {"key": "transition_out", "firstFrame": 90, "lastFrame": 119}
]}}
```

| Field | |
|---|---|
| `key` | What the app plays the animation by: 1 to 64 of `A-Z a-z 0-9 _ . -`, unique in the file |
| `firstFrame`, `lastFrame` | Comp frames, both included; frame 0 is the comp's first |
| `loop` | Whether the animation is meant to repeat (default false). Players use it unless the app says otherwise |

Or mark them in the Timeline and pass `"animations": "markers"`: every comp marker that has a
duration and a comment is an animation. The comment is the key, or the key followed by `loop`
(`idle loop`); the animation runs for the marker's duration, at most to the end of the comp.
Markers without a duration or without a comment are left alone. In the export dialog this is
Animations ▸ Comp markers with a duration.

The animations are stored in the order given (markers: in time order), and the player plays the
first one unless the app names another. Ranges may overlap: a frame two animations show is
rendered and stored once. Sprites are shared too, and atlases are grouped by the animations that
draw from them, so a player can drop the textures of an animation it no longer plays. With
`everyNthFrame`, each animation keeps every n-th frame of its range from its first frame. A key
that doesn't fit, a key used twice or a range outside the comp is an error before anything is
rendered.

The result lists them (`animations`: key, `frameCount`, `loop` and `atlases` of each).

## How layers are built

Every frame is rendered as usual. The exporter then tries to describe it as layers: each visible
layer's pixels after its source, masks and effects (before its transform) become a sprite, drawn
with the layer's transform and opacity. Sprites with the same pixels are stored once.

The layered frame is drawn the way players draw it and compared with the rendered frame. The
frame is stored as one picture instead when they differ, or when it holds something a list of
transformed sprites can't express:

- 3D layers, collapsed precomps, adjustment layers;
- blend modes other than Normal, track mattes, Preserve Transparency;
- layer styles, motion blur.

The command's result counts the frames of each kind (`layeredFrames`, `flattenedFrames` with the
reasons, `emptyFrames`), so you can see which layers to simplify for a smaller file. A precomp
layer that isn't collapsed is one sprite per look of the precomp: precompose a part that only
moves to have it stored once.

## Keys

A file uses the **official key**, which the format's players have built in, unless you pass a
custom one. EffectCraft does not ship the official key. Save it once in the settings folder:

```json
{"cmd": "file.setWanimKey", "params": {"key": "<64 hexadecimal characters>"}}
```

It is kept as `wanim_official_key.txt` in the [settings folder](preferences.md) and never echoed
back.

Or build it into your own binary: compile with the key in `EFFECTCRAFT_WANIM_OFFICIAL_KEY`, and
exports use it when the settings folder has no key. The key stays out of the source, but anyone
with that binary has it, so don't hand out such a build.

```sh
EFFECTCRAFT_WANIM_OFFICIAL_KEY=<64 hexadecimal characters> cargo build --release -p effectcraft -p effectcraft-cli
```

A custom `key` comes first, then the settings folder's key, then the built-in one. To write a file only your own app can open, pass `key` to `file.exportWanim`: the file's
header then says it uses a custom key, and the app playing it has to supply that key.

## Parameters

| Parameter | Default | |
|---|---|---|
| `path` | (asked) | The `.wanim` file to write |
| `comp` | the active comp | Id or name |
| `version` | 3 | 3, 2 or 1 |
| `layers` | true | Version 3: describe frames as layers; false stores a picture per frame. Version 2 always has layers, version 1 never |
| `animations` | one animation, `main` | Version 3: a list of `{key, firstFrame, lastFrame, loop?}`, or `"markers"` (see [Animations](#animations)) |
| `atlasFormat` | `smallest` | Version 3: `smallest` (each atlas as PNG or lossless WebP, whichever is fewer bytes), `png` or `webp` |
| `scale` | 1 | Output size × scale (above 0, at most 4) |
| `everyNthFrame` | 1 | Keep every n-th frame; the file's frame rate is divided by n |
| `workArea` | false | Export the work area instead of the whole comp (not together with `animations`) |
| `atlasSize` | 4096; version 3: 2048, or 4096 when a sprite is larger than 1024 | Largest atlas side, 256 to 4096 |
| `key` | the official key | A custom key, 64 hexadecimal characters |

The export runs on the CPU renderer, so every machine writes the same pixels. A sprite larger than
the atlas size is an error: export at a smaller `scale`.
