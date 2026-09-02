#!/usr/bin/env python3
"""Normalize a generated horizontal frame strip into a SakiPet clip."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

from PIL import Image

from extract_strip_frames import (
    component_frame_groups,
    component_group_image,
    extract_slot_frames,
    extract_stable_slot_frames,
    fit_to_cell,
    remove_chroma_background,
)

CELL_WIDTH = 192
CELL_HEIGHT = 208


def parse_hex_color(value: str) -> tuple[int, int, int]:
    if not re.fullmatch(r"#[0-9a-fA-F]{6}", value):
        raise SystemExit(f"invalid chroma key color: {value}; expected #RRGGBB")
    return tuple(int(value[index : index + 2], 16) for index in (1, 3, 5))


def parse_durations(value: str | None, frame_count: int) -> list[int]:
    if not value:
        return [180] * frame_count
    durations = [int(item.strip()) for item in value.split(",") if item.strip()]
    if len(durations) != frame_count or any(duration < 30 or duration > 5000 for duration in durations):
        raise SystemExit(
            f"durations must contain exactly {frame_count} values between 30 and 5000 ms"
        )
    return durations


def clear_transparent_rgb(image: Image.Image) -> Image.Image:
    rgba = image.convert("RGBA")
    data = bytearray(rgba.tobytes())
    for index in range(0, len(data), 4):
        if data[index + 3] == 0:
            data[index : index + 3] = b"\x00\x00\x00"
    return Image.frombytes("RGBA", rgba.size, bytes(data))


def save_image(image: Image.Image, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.suffix.lower() == ".webp":
        image.save(path, format="WEBP", lossless=True, quality=100, method=6, exact=True)
    else:
        image.save(path)


def extract_frames(
    strip: Image.Image,
    frame_count: int,
    chroma_key: tuple[int, int, int],
    threshold: float,
    method: str,
) -> tuple[list[Image.Image], str]:
    cleaned = remove_chroma_background(strip, chroma_key, threshold)
    groups = component_frame_groups(cleaned, frame_count)

    if method == "stable-slots" or (method == "auto" and groups is not None):
        if groups is not None:
            return extract_stable_slot_frames(cleaned, frame_count), "stable-slots"
        if method == "stable-slots":
            return extract_slot_frames(cleaned, frame_count), "slots-fallback"

    if method in {"auto", "components"} and groups is not None:
        return [fit_to_cell(component_group_image(cleaned, group)) for group in groups], "components"
    if method == "components":
        raise SystemExit(f"could not find {frame_count} separated sprite components")
    return extract_slot_frames(cleaned, frame_count), "slots"


def save_preview(frames: list[Image.Image], durations: list[int], output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    frames[0].save(
        output,
        save_all=True,
        append_images=frames[1:],
        duration=durations,
        loop=0,
        disposal=2,
        optimize=False,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--frames", type=int, required=True)
    parser.add_argument("--chroma-key", required=True)
    parser.add_argument("--key-threshold", type=float, default=96.0)
    parser.add_argument(
        "--method",
        choices=("auto", "components", "slots", "stable-slots"),
        default="stable-slots",
    )
    parser.add_argument("--durations", help="comma-separated frame durations in milliseconds")
    parser.add_argument("--frames-dir")
    parser.add_argument("--preview")
    parser.add_argument("--json-out")
    args = parser.parse_args()

    if not 1 <= args.frames <= 32:
        raise SystemExit("frames must be between 1 and 32")
    input_path = Path(args.input).expanduser().resolve()
    with Image.open(input_path) as opened:
        source = opened.convert("RGBA")
    frames, used_method = extract_frames(
        source,
        args.frames,
        parse_hex_color(args.chroma_key),
        args.key_threshold,
        args.method,
    )
    durations = parse_durations(args.durations, args.frames)

    if len(frames) != args.frames:
        raise SystemExit(f"expected {args.frames} frames, extracted {len(frames)}")
    output = Image.new("RGBA", (CELL_WIDTH * args.frames, CELL_HEIGHT), (0, 0, 0, 0))
    for index, frame in enumerate(frames):
        frame = clear_transparent_rgb(frame)
        output.alpha_composite(frame, (index * CELL_WIDTH, 0))
        if args.frames_dir:
            frame_path = Path(args.frames_dir).expanduser().resolve() / f"{index:02d}.png"
            save_image(frame, frame_path)
    output = clear_transparent_rgb(output)
    output_path = Path(args.output).expanduser().resolve()
    save_image(output, output_path)

    if args.preview:
        save_preview(frames, durations, Path(args.preview).expanduser().resolve())

    result = {
        "ok": True,
        "input": str(input_path),
        "output": str(output_path),
        "sourceSize": list(source.size),
        "outputSize": list(output.size),
        "frames": args.frames,
        "durations": durations,
        "method": used_method,
        "chromaKey": args.chroma_key.upper(),
        "preview": str(Path(args.preview).expanduser().resolve()) if args.preview else None,
    }
    if args.json_out:
        report_path = Path(args.json_out).expanduser().resolve()
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
