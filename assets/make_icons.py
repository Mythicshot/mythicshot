"""Build assets/mythicshot.ico from the two pieces of artwork beside it.
Sizes under 48 pixels come from the simplified picture (mythicshot-logo-small.png),
48 and up from the full one (mythicshot-logo.png). The app reads its window and
tray icons out of the same .ico, so this is the only generated icon file.
Run from the repo root: python assets/make_icons.py
Then, for the installer pictures: python installer/make_images.py"""
import io
import struct

from PIL import Image

SMALL_SIZES = [16, 20, 24, 32, 40]
FULL_SIZES = [48, 64, 128, 256]


def square(path):
    """The picture cut to what is drawn and centred on a transparent square."""
    art = Image.open(path).convert("RGBA")
    art = art.crop(art.getchannel("A").getbbox())
    side = max(art.size)
    out = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    out.paste(art, ((side - art.width) // 2, (side - art.height) // 2))
    return out


def shrink(art, size):
    """Resize with the colour weighted by alpha, so see-through edges do not turn dark."""
    return art.convert("RGBa").resize((size, size), Image.LANCZOS).convert("RGBA")


def png(image):
    data = io.BytesIO()
    image.save(data, "PNG", optimize=True)
    return data.getvalue()


def main():
    small, full = square("assets/mythicshot-logo-small.png"), square("assets/mythicshot-logo.png")
    images = [(s, png(shrink(small, s))) for s in SMALL_SIZES] + [(s, png(shrink(full, s))) for s in FULL_SIZES]
    # An .ico is a list of entries (16 bytes each) followed by the pictures, here each a PNG.
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for size, data in images:
        out += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    with open("assets/mythicshot.ico", "wb") as f:
        f.write(out + b"".join(data for _, data in images))


if __name__ == "__main__":
    main()
