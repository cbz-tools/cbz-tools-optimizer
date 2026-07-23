from __future__ import annotations

import argparse
from pathlib import Path
from typing import Iterable

from PIL import Image, ImageEnhance, ImageFilter


SIZES = (16, 24, 32, 48, 64, 128, 256)


def render_size(src: Image.Image, size: int) -> Image.Image:
    """Render one icon size while retaining alpha and tiny-size legibility."""
    img = src.resize((size, size), Image.Resampling.LANCZOS)
    if size <= 32:
        img = ImageEnhance.Contrast(img).enhance(1.08)
        img = img.filter(ImageFilter.UnsharpMask(radius=0.8, percent=130, threshold=2))
    return img


def save_preview(renders: Iterable[Image.Image], out_path: Path) -> None:
    items = list(renders)
    gap = 8
    width = sum(item.width for item in items) + gap * (len(items) - 1)
    height = max(item.height for item in items)
    preview = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    x = 0
    for item in items:
        y = (height - item.height) // 2
        preview.paste(item, (x, y), item)
        x += item.width + gap
    preview.save(out_path)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Generate a Windows multi-size .ico and window PNG from source PNG"
    )
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--png-out", required=True, type=Path)
    parser.add_argument("--ico-out", required=True, type=Path)
    parser.add_argument("--preview-out", required=True, type=Path)
    args = parser.parse_args()

    src = Image.open(args.input).convert("RGBA")
    if src.width != src.height:
        parser.error("--input must be a square PNG")

    renders = [render_size(src, size) for size in SIZES]

    args.png_out.parent.mkdir(parents=True, exist_ok=True)
    renders[-1].save(args.png_out)

    args.ico_out.parent.mkdir(parents=True, exist_ok=True)
    renders[-1].save(args.ico_out, format="ICO", sizes=[(size, size) for size in SIZES])

    args.preview_out.parent.mkdir(parents=True, exist_ok=True)
    save_preview(renders[:3], args.preview_out)


if __name__ == "__main__":
    main()
