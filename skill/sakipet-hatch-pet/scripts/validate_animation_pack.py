#!/usr/bin/env python3
"""Validate an optional SakiPet full-body frame-animation pack."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from PIL import Image


CELL_WIDTH = 192
CELL_HEIGHT = 208
MAX_PACK_BYTES = 2 * 1024 * 1024
MAX_CLIP_BYTES = 20 * 1024 * 1024
MAX_FRAMES = 32
FORMAT = "sakipet-frame-pack"
VERSION = 1
SAFE_ID = re.compile(r"^[-a-zA-Z0-9_]{1,64}$")
STATES = {
    "idle",
    "running-right",
    "running-left",
    "waving",
    "jumping",
    "failed",
    "waiting",
    "running",
    "review",
}


def fail(message: str) -> None:
    raise ValueError(message)


def validate(root: Path) -> dict[str, object]:
    manifest_path = root / "animations.json"
    if not manifest_path.is_file():
        return {"ok": True, "present": False, "clips": 0, "errors": []}
    if manifest_path.stat().st_size > MAX_PACK_BYTES:
        fail("animations.json exceeds 2 MB")

    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read animations.json: {error}")
    if not isinstance(manifest, dict):
        fail("animations.json must contain an object")
    if manifest.get("format") != FORMAT or manifest.get("version") != VERSION:
        fail("unsupported animation pack format or version")
    if manifest.get("cellWidth") != CELL_WIDTH or manifest.get("cellHeight") != CELL_HEIGHT:
        fail("cellWidth and cellHeight must be 192 and 208")

    clips = manifest.get("clips")
    if not isinstance(clips, dict) or not 1 <= len(clips) <= 64:
        fail("clips must contain between 1 and 64 entries")

    checked: list[str] = []
    for clip_id, clip in clips.items():
        if not isinstance(clip_id, str) or not SAFE_ID.fullmatch(clip_id):
            fail(f"unsafe clip id: {clip_id!r}")
        if not isinstance(clip, dict):
            fail(f"clip {clip_id} must contain an object")
        path = clip.get("path")
        if (
            not isinstance(path, str)
            or not path.startswith("animations/")
            or Path(path).is_absolute()
            or any(part in {"", ".", ".."} for part in path.split("/"))
        ):
            fail(f"clip {clip_id} has an unsafe path")
        frames = clip.get("frames")
        durations = clip.get("durations")
        if not isinstance(frames, int) or not 1 <= frames <= MAX_FRAMES:
            fail(f"clip {clip_id} frame count must be 1..{MAX_FRAMES}")
        if (
            not isinstance(durations, list)
            or len(durations) != frames
            or any(not isinstance(duration, int) or not 30 <= duration <= 5000 for duration in durations)
        ):
            fail(f"clip {clip_id} has invalid durations")
        if not isinstance(clip.get("loop"), bool):
            fail(f"clip {clip_id} loop must be boolean")
        loop_start = clip.get("loopStart")
        if loop_start is not None and (not isinstance(loop_start, int) or not 0 <= loop_start < frames):
            fail(f"clip {clip_id} has invalid loopStart")
        if clip.get("type") not in {"mode", "gesture"}:
            fail(f"clip {clip_id} type must be mode or gesture")
        for field in ("returnTo", "fallback"):
            value = clip.get(field)
            if value is not None and value not in STATES:
                fail(f"clip {clip_id} has invalid {field}: {value!r}")

        clip_path = root / path
        if not clip_path.is_file():
            fail(f"clip {clip_id} is missing: {path}")
        if clip_path.stat().st_size == 0 or clip_path.stat().st_size > MAX_CLIP_BYTES:
            fail(f"clip {clip_id} is empty or exceeds 20 MB")
        try:
            with Image.open(clip_path) as image:
                image.load()
                if image.width != CELL_WIDTH * frames or image.height != CELL_HEIGHT:
                    fail(
                        f"clip {clip_id} is {image.width}x{image.height}; "
                        f"expected {CELL_WIDTH * frames}x{CELL_HEIGHT}"
                    )
                if "A" not in image.getbands():
                    fail(f"clip {clip_id} must have an alpha channel")
        except (OSError, ValueError) as error:
            fail(f"cannot decode clip {clip_id}: {error}")
        checked.append(clip_id)

    return {"ok": True, "present": True, "clips": len(checked), "clipIds": checked, "errors": []}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pet_dir", type=Path, help="pet package directory")
    parser.add_argument("--json-out", type=Path, help="optional validation report path")
    args = parser.parse_args()
    try:
        report = validate(args.pet_dir.resolve())
    except ValueError as error:
        report = {"ok": False, "present": (args.pet_dir / "animations.json").is_file(), "errors": [str(error)]}
    if args.json_out:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
