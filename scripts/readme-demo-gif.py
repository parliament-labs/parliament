#!/usr/bin/env python3
"""Make docs/screenshots/readme-demo.gif from the frames the README demo
spec captures (tests/playwright/screenshots/05-readme-demo.spec.ts writes
them, with frames.json saying how long each shows, to
tests/playwright/screenshots/_demo/).

Needs Pillow (`pip install pillow`). One palette for every frame and no
dithering, so the text stays crisp and a frame differs from the one
before only where the UI changed — Pillow then stores just that part.
"""

import json
import pathlib

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent
FRAMES = ROOT / "tests" / "playwright" / "screenshots" / "_demo"
OUT = ROOT / "docs" / "screenshots" / "readme-demo.gif"


def main() -> None:
    frames = json.loads((FRAMES / "frames.json").read_text())
    images = [Image.open(FRAMES / f["file"]).convert("RGB") for f in frames]

    # The palette from every frame that holds — the results, where the
    # colours are — stacked into one image.
    held = [im for im, f in zip(images, frames) if f["ms"] > 500] or images
    width, height = held[0].size
    sheet = Image.new("RGB", (width, height * len(held)))
    for i, im in enumerate(held):
        sheet.paste(im, (0, height * i))
    palette = sheet.quantize(colors=256, method=Image.Quantize.MEDIANCUT)

    gif = [im.quantize(palette=palette, dither=Image.Dither.NONE) for im in images]
    gif[0].save(
        OUT,
        save_all=True,
        append_images=gif[1:],
        duration=[f["ms"] for f in frames],
        loop=0,
        optimize=False,
    )
    seconds = sum(f["ms"] for f in frames) / 1000
    print(f"{OUT.relative_to(ROOT)}: {len(frames)} frames, {seconds:.1f} s, {OUT.stat().st_size / 1e6:.2f} MB")


if __name__ == "__main__":
    main()
