// Renders the image link previews show for the site's pages, from
// social-card.svg beside this script, into public/og.png. Run it again
// after changing the SVG, such as when the logo changes:
//
//   node scripts/social-card.mjs
//
// The text is drawn with this machine's fonts, so the PNG is committed
// rather than rendered on every build.

import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const svg = fileURLToPath(new URL('social-card.svg', import.meta.url));
const png = fileURLToPath(new URL('../public/og.png', import.meta.url));
const info = await sharp(svg).png({ compressionLevel: 9 }).toFile(png);
console.log(`wrote public/og.png, ${info.width}×${info.height}, ${info.size} bytes`);
