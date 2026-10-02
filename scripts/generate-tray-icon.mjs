#!/usr/bin/env node
// Rasterises assets/mullion-helper.svg into src-tauri/icons/tray-64.png.
// No SVG renderer is assumed to be installed, so this reads the <rect> tiles
// straight from the SVG (axis-aligned, rounded, 32-unit viewBox) and does its
// own supersampled coverage. Run by hand when the logo changes:
//   npm run icons:tray
import { readFileSync, writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const SIZE = 64;
const SUPERSAMPLE = 4;

const svg = readFileSync(join(root, "assets", "mullion-helper.svg"), "utf8");
const viewBox = /viewBox="0 0 (\d+(?:\.\d+)?) (\d+(?:\.\d+)?)"/.exec(svg);
if (!viewBox) throw new Error("mullion-helper.svg has no 0 0 W H viewBox");
const scale = SIZE / Number(viewBox[1]);

const attr = (tag, name) => {
  const match = new RegExp(`\\b${name}="([^"]*)"`).exec(tag);
  return match ? match[1] : undefined;
};
const hex = (value) => [1, 3, 5].map((i) => parseInt(value.slice(i, i + 2), 16));

const fillOf = (tag) => {
  const fill = attr(tag, "fill");
  if (!/^#[0-9a-fA-F]{6}$/.test(fill ?? "")) {
    throw new Error(`${tag} needs a #rrggbb fill, got ${JSON.stringify(fill)}`);
  }
  return hex(fill);
};

const rects = [...svg.matchAll(/<rect\b[^>]*>/g)].map(([tag]) => ({
  x: Number(attr(tag, "x")) * scale,
  y: Number(attr(tag, "y")) * scale,
  w: Number(attr(tag, "width")) * scale,
  h: Number(attr(tag, "height")) * scale,
  r: Number(attr(tag, "rx") ?? 0) * scale,
  color: fillOf(tag),
}));
if (rects.length === 0) throw new Error("no <rect> tiles found in the SVG");

function inRoundedRect(px, py, { x, y, w, h, r }) {
  if (px < x || px > x + w || py < y || py > y + h) return false;
  const cx = px < x + r ? x + r : px > x + w - r ? x + w - r : px;
  const cy = py < y + r ? y + r : py > y + h - r ? y + h - r : py;
  return (px - cx) ** 2 + (py - cy) ** 2 <= r * r;
}

const samples = SUPERSAMPLE * SUPERSAMPLE;
const rgba = Buffer.alloc(SIZE * SIZE * 4);
for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    // Tiles don't overlap, so each sample hits at most one rect.
    const sum = [0, 0, 0];
    let covered = 0;
    for (let sy = 0; sy < SUPERSAMPLE; sy++) {
      for (let sx = 0; sx < SUPERSAMPLE; sx++) {
        const px = x + (sx + 0.5) / SUPERSAMPLE;
        const py = y + (sy + 0.5) / SUPERSAMPLE;
        const rect = rects.find((candidate) => inRoundedRect(px, py, candidate));
        if (!rect) continue;
        covered++;
        for (let c = 0; c < 3; c++) sum[c] += rect.color[c];
      }
    }
    const offset = (y * SIZE + x) * 4;
    if (covered > 0) {
      for (let c = 0; c < 3; c++) rgba[offset + c] = Math.round(sum[c] / covered);
      rgba[offset + 3] = Math.round((255 * covered) / samples);
    }
  }
}

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

const header = Buffer.alloc(13);
header.writeUInt32BE(SIZE, 0);
header.writeUInt32BE(SIZE, 4);
header[8] = 8; // bit depth
header[9] = 6; // RGBA
const stride = SIZE * 4;
const raw = Buffer.alloc((stride + 1) * SIZE); // filter byte 0 per scanline
for (let y = 0; y < SIZE; y++) {
  rgba.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
}

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", header),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

const out = join(root, "src-tauri", "icons", `tray-${SIZE}.png`);
writeFileSync(out, png);
console.log(`wrote ${out} (${png.length} bytes)`);
