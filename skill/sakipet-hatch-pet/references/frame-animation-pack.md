# SakiPet Frame Animation Pack

SakiPet keeps the Codex V2 package contract unchanged and adds optional full-body frame clips beside it. The base `pet.json` must not gain extension fields and `spritesheet.webp` must remain the exact `1536x2288` V2 atlas.

## Package layout

```text
pet/
├── pet.json
├── spritesheet.webp
├── character.json
├── animations.json
└── animations/
    ├── sleep.webp
    ├── yawn.webp
    └── celebrate.webp
```

`animations.json` is optional. A runtime that does not understand it must continue using the V2 atlas only. SakiPet discovers it next to `pet.json`; do not add an `animations` field to `pet.json`.

## Manifest schema

```json
{
  "format": "sakipet-frame-pack",
  "version": 1,
  "cellWidth": 192,
  "cellHeight": 208,
  "clips": {
    "sleep": {
      "path": "animations/sleep.webp",
      "frames": 6,
      "durations": [300, 180, 180, 300, 180, 500],
      "loop": true,
      "loopStart": 3,
      "type": "mode",
      "fallback": "waiting"
    },
    "yawn": {
      "path": "animations/yawn.webp",
      "frames": 6,
      "durations": [140, 140, 220, 180, 180, 320],
      "loop": false,
      "type": "gesture",
      "returnTo": "idle"
    }
  }
}
```

Each clip file is a horizontal strip: `frames * 192` pixels wide by `208` pixels high. Every frame is a complete, transparent-background full-body sprite. Keep the frame count at 1–32 and keep every duration between 30 and 5000 ms.

`mode` clips are persistent states such as `sleep`. A mode may use `loopStart` to play an entry section once and then repeat only the stable tail from that zero-based frame; omit it to loop all frames. `gesture` clips play once and return to the current base state unless `returnTo` is specified. `fallback` names a V2 standard state to use when the clip cannot load.

## Generation and QA

Generate each clip as one coherent strip with `$imagegen`, grounded by the canonical base and the approved V2 contact sheet. Do not create new actions by rotating, skewing, warping, recoloring, tiling, or procedurally compositing the V2 sprite. The only normal deterministic derivation retained from the base workflow is the approved `running-left` mirror.

For every clip:

- keep the pet identity, style, palette, face, proportions, materials, and props locked;
- preserve a natural anchor such as feet, haunches, or lower torso unless the action intentionally changes it;
- include an understandable entry, action, and exit phase for one-shot gestures;
- use 4–8 frames for micro-actions and 6–12 frames for larger gestures unless the motion genuinely needs more;
- reject clipping, detached effects, transparent holes inside the body, guide marks, or effectively identical frames;
- render a normal-size preview and inspect cadence, baseline, scale, and loop closure.

Use `scripts/assemble_frame_clip.py` for the deterministic source-strip step. It removes the
approved chroma key, extracts separated poses, normalizes a shared baseline into `192x208`
cells, writes the exact horizontal strip size, and can emit a GIF preview. It must only process
an already-generated visual output; it must not invent or redraw a pose.

For cat-like pets, prefer ears, eyes, tail, paws, and small upper-body changes for micro-actions. Keep the sprite itself free of speech bubbles, floating symbols, shadows, and UI effects; those belong to the application effect layer.

## Runtime semantics

Resolve animation in this order:

1. dragging or carrying;
2. social or system lock;
3. an active one-shot clip;
4. a persistent mode clip;
5. a V2 standard state;
6. the 16-cell V2 look direction when no full-body clip owns the frame.

Full-body clips own the complete cell, so look direction is temporarily held during a clip and restored afterward. A gesture that needs simultaneous directional variants must provide those variants as actual frame clips; never rotate the completed sprite at runtime.

## Validation

The package validator must check the sidecar format, safe `animations/` paths, frame counts, duration arrays, exact strip dimensions, decodability, file size, and all transparent-frame invariants. Invalid optional packs are ignored with a warning and the pet falls back to the unchanged V2 package.
