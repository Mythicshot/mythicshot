"""Draw the installer's banner and side images from assets/mythicshot-256.png.
Run from the repo root: python installer/make_images.py"""
from PIL import Image, ImageDraw, ImageFont

icon = Image.open("assets/mythicshot-256.png").convert("RGBA")
DARK = (30, 31, 34)
ACCENT = (232, 89, 60)

# Top banner, 493x58: text sits on the left, so keep that white.
banner = Image.new("RGB", (493, 58), "white")
banner.paste(icon.resize((44, 44), Image.LANCZOS), (437, 7), icon.resize((44, 44), Image.LANCZOS))
banner.save("installer/images/banner.bmp")

# Welcome and finish pages, 493x312: a dark panel on the left 164 px, text on the right.
side = Image.new("RGB", (493, 312), "white")
d = ImageDraw.Draw(side)
d.rectangle([0, 0, 163, 311], fill=DARK)
d.rectangle([160, 0, 163, 311], fill=ACCENT)
big = icon.resize((96, 96), Image.LANCZOS)
side.paste(big, (34, 80), big)
try:
    font = ImageFont.truetype("segoeuib.ttf", 18)
except OSError:
    font = ImageFont.load_default()
w = d.textlength("Mythicshot", font=font)
d.text(((160 - w) / 2, 190), "Mythicshot", font=font, fill="white")
side.save("installer/images/dialog.bmp")
