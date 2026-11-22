// Makes the app icon and the marks the installer pictures use, from the brand's marks in
// assets/brand/. Run it again only when a mark changes; the results are committed.
//
//   node scripts/make-brand-art.mjs <the folder of the sharp package, or a package.json beside a node_modules that holds it>
//
// sharp is not a dependency of this repository: the script borrows it from wherever it is already
// installed, named by the argument. Which mark each picture comes from follows the brand's size
// rule: the small mark below 48 px, the standard mark from 48 to 96 px, the rich mark above 96 px.
//
//   assets/mythicshot.ico               16, 20, 24, 32, 40 px from mythicshot-mark-small.svg,
//                                       48, 64 px from mythicshot-mark.svg,
//                                       128, 256 px from mythicshot-mark-rich.svg; each a PNG, transparent
//   installer/images/banner-mark.png    44 px from mythicshot-mark-small.svg, for the wizard's banner
//   installer/images/side-mark.png      104 px from mythicshot-mark-rich.svg, for its side panel
//
// Then draw the wizard's bitmaps from these: python installer/make_images.py
import { createRequire } from 'node:module';
import { readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const from = process.argv[2];
if (!from) {
  console.error('Name the folder of an installed sharp package, for example: node scripts/make-brand-art.mjs C:/somewhere/node_modules/sharp');
  process.exit(1);
}
const at = resolve(from);
const sharp = statSync(at).isDirectory() ? createRequire(join(at, 'index.js'))(at) : createRequire(at)('sharp');
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const brand = join(root, 'assets', 'brand');

export const ICON_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256];
const markFor = (size) => join(brand, size < 48 ? 'mythicshot-mark-small.svg' : size <= 96 ? 'mythicshot-mark.svg' : 'mythicshot-mark-rich.svg');

// The mark drawn over the whole square, see-through around it. Rendered at four times the size
// and shrunk, so the gaps between the facets stay clean at small sizes.
async function render(size) {
  const svg = readFileSync(markFor(size));
  const mark = await sharp(svg, { density: Math.max(72, Math.ceil((72 * size) / 100) * 4) }).resize(size, size).png().toBuffer();
  return sharp({ create: { width: size, height: size, channels: 4, background: { r: 0, g: 0, b: 0, alpha: 0 } } })
    .composite([{ input: mark, left: 0, top: 0 }])
    .png({ compressionLevel: 9 })
    .toBuffer();
}

// An .ico file whose entries are PNG pictures, which Windows reads at every size (Vista and later).
function ico(images) {
  const header = Buffer.alloc(6 + 16 * images.length);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(images.length, 4);
  let offset = header.length;
  images.forEach(({ size, png }, i) => {
    const entry = 6 + 16 * i;
    // A side of 256 is written as 0.
    header.writeUInt8(size >= 256 ? 0 : size, entry);
    header.writeUInt8(size >= 256 ? 0 : size, entry + 1);
    header.writeUInt8(0, entry + 2);
    header.writeUInt8(0, entry + 3);
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(png.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += png.length;
  });
  return Buffer.concat([header, ...images.map((image) => image.png)]);
}

const images = [];
for (const size of ICON_SIZES) images.push({ size, png: await render(size) });
writeFileSync(join(root, 'assets', 'mythicshot.ico'), ico(images));
writeFileSync(join(root, 'installer', 'images', 'banner-mark.png'), await render(44));
writeFileSync(join(root, 'installer', 'images', 'side-mark.png'), await render(104));
console.log(`Wrote assets/mythicshot.ico (${ICON_SIZES.join(', ')}), installer/images/banner-mark.png and installer/images/side-mark.png.`);
