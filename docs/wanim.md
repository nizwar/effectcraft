# `.wanim` export

File ▸ Export ▸ WANIM… (`file.exportWanim`) writes the active comp as a `.wanim` sprite
animation: texture atlases plus a manifest of what to draw in each frame, encrypted with
AES-256-GCM so that only players holding the key can read it. The crate is
`crates/wanim` (`effectcraft-wanim`).

| Version | What a frame stores | Use it for |
|---|---|---|
| 2 (default) | A list of sprites, each drawn with a matrix and an opacity | Motion graphics: a layer that only moves, turns, scales or fades is stored once |
| 1 | One sprite: the frame cropped to its pixels | Footage-like content where every frame looks different anyway |

## How version 2 is built

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
| `version` | 2 | 1 or 2 |
| `scale` | 1 | Output size × scale (above 0, at most 4) |
| `everyNthFrame` | 1 | Keep every n-th frame; the file's frame rate is divided by n |
| `workArea` | false | Export the work area instead of the whole comp |
| `atlasSize` | 4096 | Largest atlas side, 256 to 4096 |
| `key` | the official key | A custom key, 64 hexadecimal characters |

The export runs on the CPU renderer, so every machine writes the same pixels. A sprite larger than
the atlas size is an error: export at a smaller `scale`.
