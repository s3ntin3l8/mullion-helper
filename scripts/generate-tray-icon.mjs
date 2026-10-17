#!/usr/bin/env node
// Rasterises assets/mullion-helper.svg into src-tauri/icons/tray-64.png.
// No SVG renderer is assumed to be installed, so this reads the <rect> tiles
// straight from the SVG (axis-aligned, rounded, 32-unit viewBox) and does its
// own supersampled coverage. Run by hand when the logo changes:
//   npm run icons:tray
// `--check` (npm run check:icons, part of lint) re-renders and compares decoded
// RGBA pixels against the committed PNG instead of writing it; pixels, not
// bytes, because deflate output differs across zlib versions.
import { readFileSync, writeFileSync } from "node:fs";
import { deflateSync, inflateSync } from "node:zlib";
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

function decodePng(buffer) {
  const stale = (why) => new Error(`tray-${SIZE}.png ${why} - run npm run icons:tray`);
  if (!buffer.subarray(0, 8).equals(png.subarray(0, 8))) throw stale("is not a PNG");
  let ihdr;
  const idat = [];
  for (let at = 8; at + 8 <= buffer.length; ) {
    const length = buffer.readUInt32BE(at);
    const type = buffer.toString("ascii", at + 4, at + 8);
    const data = buffer.subarray(at + 8, at + 8 + length);
    if (type === "IHDR") ihdr = data;
    else if (type === "IDAT") idat.push(data);
    at += 12 + length;
  }
  if (!ihdr || idat.length === 0) throw stale("is missing IHDR/IDAT");
  if (ihdr.readUInt32BE(0) !== SIZE || ihdr.readUInt32BE(4) !== SIZE || ihdr[8] !== 8 || ihdr[9] !== 6 || ihdr[12] !== 0) {
    throw stale(`is not a non-interlaced ${SIZE}x${SIZE} 8-bit RGBA image`);
  }
  const data = inflateSync(Buffer.concat(idat));
  const pixels = Buffer.alloc(stride * SIZE);
  for (let y = 0; y < SIZE; y++) {
    const filter = data[y * (stride + 1)];
    for (let i = 0; i < stride; i++) {
      const left = i >= 4 ? pixels[y * stride + i - 4] : 0;
      const up = y > 0 ? pixels[(y - 1) * stride + i] : 0;
      const upLeft = y > 0 && i >= 4 ? pixels[(y - 1) * stride + i - 4] : 0;
      const p = left + up - upLeft;
      const [pa, pb, pc] = [Math.abs(p - left), Math.abs(p - up), Math.abs(p - upLeft)];
      const predictor = [
        0,
        left,
        up,
        (left + up) >> 1,
        pa <= pb && pa <= pc ? left : pb <= pc ? up : upLeft,
      ][filter];
      if (predictor === undefined) throw stale(`uses unknown PNG filter ${filter}`);
      pixels[y * stride + i] = (data[y * (stride + 1) + 1 + i] + predictor) & 0xff;
    }
  }
  return pixels;
}

const out = join(root, "src-tauri", "icons", `tray-${SIZE}.png`);
if (process.argv.includes("--check")) {
  const committed = decodePng(readFileSync(out));
  let differing = 0;
  for (let i = 0; i < rgba.length; i += 4) {
    if (!rgba.subarray(i, i + 4).equals(committed.subarray(i, i + 4))) differing++;
  }
  if (differing > 0) {
    console.error(`tray-${SIZE}.png is stale (${differing} pixels differ from mullion-helper.svg) - run npm run icons:tray`);
    process.exit(1);
  }
  console.log(`tray-${SIZE}.png matches mullion-helper.svg.`);
} else {
  writeFileSync(out, png);
  console.log(`wrote ${out} (${png.length} bytes)`);
}
