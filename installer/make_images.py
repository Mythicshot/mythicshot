"""Draw the installer's banner and side images from the artwork in assets/:
the simplified picture for the 44 pixel banner mark, the full one for the 96 pixel side panel.
Run from the repo root: python installer/make_images.py"""
import sys

from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, "assets")
from make_icons import shrink, square  # noqa: E402

DARK = (30, 31, 34)
ACCENT = (232, 89, 60)

# Top banner, 493x58: text sits on the left, so keep that white.
banner = Image.new("RGB", (493, 58), "white")
mark = shrink(square("assets/mythicshot-logo-small.png"), 44)
banner.paste(mark, (437, 7), mark)
banner.save("installer/images/banner.bmp")

# Welcome and finish pages, 493x312: a dark panel on the left 164 px, text on the right.
side = Image.new("RGB", (493, 312), "white")
d = ImageDraw.Draw(side)
d.rectangle([0, 0, 163, 311], fill=DARK)
d.rectangle([160, 0, 163, 311], fill=ACCENT)
big = shrink(square("assets/mythicshot-logo.png"), 96)
side.paste(big, (34, 80), big)
try:
    font = ImageFont.truetype("segoeuib.ttf", 18)
except OSError:
    font = ImageFont.load_default()
w = d.textlength("Mythicshot", font=font)
d.text(((160 - w) / 2, 190), "Mythicshot", font=font, fill="white")
side.save("installer/images/dialog.bmp")
