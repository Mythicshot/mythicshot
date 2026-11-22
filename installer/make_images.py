"""Draw the installer's banner and side images in the brand.
The marks come from scripts/make-brand-art.mjs (installer/images/banner-mark.png, the small mark at
44 pixels, and installer/images/side-mark.png, the rich mark at 104 pixels); the name is set in
Cinzel capitals, weight 600, from the font in assets/fonts/cinzel.
Run from the repo root: python installer/make_images.py"""
from PIL import Image, ImageDraw, ImageFont

# Brand colour tokens, dark values: ground for the panel, gold for the name (text on a dark ground).
GROUND = (0x16, 0x12, 0x1C)  # ground (dark)
GOLD = (0xF5, 0xB8, 0x41)  # accent-text (dark)
# Light surface-raised: the wizard sets its own black text over the rest of both pictures.
PAPER = (0xFF, 0xFF, 0xFF)  # surface-raised (light)

# Top banner, 493x58: the wizard's text sits on the left, so that stays light; the small mark on the right.
banner = Image.new("RGB", (493, 58), PAPER)
mark = Image.open("installer/images/banner-mark.png").convert("RGBA")
banner.paste(mark, (437, 7), mark)
banner.save("installer/images/banner.bmp")

# Welcome and finish pages, 493x312: the panel on the left 164 px on the dark ground, the wizard's text on the right.
side = Image.new("RGB", (493, 312), PAPER)
d = ImageDraw.Draw(side)
d.rectangle([0, 0, 163, 311], fill=GROUND)
big = Image.open("installer/images/side-mark.png").convert("RGBA")
side.paste(big, ((164 - big.width) // 2, 72), big)
font = ImageFont.truetype("assets/fonts/cinzel/Cinzel[wght].ttf", 18)
font.set_variation_by_axes([600])
name = "MYTHICSHOT"
# The wordmark's letter spacing, 0.04 em, drawn a letter at a time.
spacing = 0.04 * 18
widths = [d.textlength(c, font=font) for c in name]
w = sum(widths) + spacing * (len(name) - 1)
x = (164 - w) / 2
for c, cw in zip(name, widths):
    d.text((x, 196), c, font=font, fill=GOLD)
    x += cw + spacing
side.save("installer/images/dialog.bmp")
