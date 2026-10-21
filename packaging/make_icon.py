"""Draws the Quarry app icon (database glyph on a blue disc) as a multi-size .ico.

Usage: python packaging/make_icon.py crates/quarry-app/resources/quarry.ico
"""
import sys

from PIL import Image, ImageDraw

S = 1024
BLUE = (36, 101, 218, 255)
WHITE = (255, 255, 255, 255)


def draw() -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.ellipse((16, 16, S - 16, S - 16), fill=BLUE)
    w, stroke = 500, 52
    left, right = (S - w) // 2, (S + w) // 2
    top, bottom, eh = 250, 780, 160
    # Top lid, then the two lower rims, then the sides.
    d.ellipse((left, top, right, top + eh), outline=WHITE, width=stroke)
    for y in (top + (bottom - top) // 2 - eh // 2, bottom - eh):
        d.arc((left, y, right, y + eh), 0, 180, fill=WHITE, width=stroke)
    for x in (left, right - stroke):
        d.rectangle((x, top + eh // 2, x + stroke, bottom - eh // 2), fill=WHITE)
    return img


if __name__ == "__main__":
    out = sys.argv[1]
    draw().save(out, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    print("wrote", out)
