import { writeFileSync } from 'node:fs';
import { deflateSync } from 'node:zlib';

const CRC_TABLE = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

function png(canvas) {
  const { w, h, px } = canvas;
  const raw = Buffer.alloc((w * 4 + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (w * 4 + 1)] = 0;
    px.copy(raw, y * (w * 4 + 1) + 1, y * w * 4, (y + 1) * w * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0))]);
}

function canvas(w, h, bg) {
  const c = { w, h, px: Buffer.alloc(w * h * 4) };
  rect(c, 0, 0, w, h, bg);
  return c;
}

function hex(color) {
  const n = parseInt(color.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255, 255];
}

function set(c, x, y, rgba) {
  if (x < 0 || y < 0 || x >= c.w || y >= c.h) return;
  const i = (y * c.w + x) * 4;
  c.px[i] = rgba[0];
  c.px[i + 1] = rgba[1];
  c.px[i + 2] = rgba[2];
  c.px[i + 3] = rgba[3];
}

function rect(c, x, y, w, h, color, r = 0) {
  const rgba = hex(color);
  for (let j = y; j < y + h; j++) {
    for (let i = x; i < x + w; i++) {
      if (r) {
        const cx = i < x + r ? x + r : i >= x + w - r ? x + w - r - 1 : i;
        const cy = j < y + r ? y + r : j >= y + h - r ? y + h - r - 1 : j;
        if ((i - cx) ** 2 + (j - cy) ** 2 > r * r) continue;
      }
      set(c, i, j, rgba);
    }
  }
}

function circle(c, cx, cy, r, color) {
  const rgba = hex(color);
  for (let j = cy - r; j <= cy + r; j++) for (let i = cx - r; i <= cx + r; i++) if ((i - cx) ** 2 + (j - cy) ** 2 <= r * r) set(c, i, j, rgba);
}

function page(variant) {
  const c = canvas(480, 300, '#F6F2FA');
  rect(c, 0, 0, 480, 44, '#24162F');
  circle(c, 24, 22, 10, '#FFD028');
  rect(c, 44, 16, 90, 12, '#B6ADC3', 6);
  rect(c, 360, 12, 100, 20, '#3F0B69', 10);
  rect(c, 24, 68, 280, 18, '#24162F', 6);
  rect(c, 24, 96, 220, 10, '#8D829A', 5);
  rect(c, 24, 114, 250, 10, '#8D829A', 5);
  rect(c, 24, 144, 432, 120, '#FFFFFF', 12);
  rect(c, 40, 160, 80, 80, '#DBB6FF', 10);
  rect(c, 136, 166, 180, 12, '#393344', 6);
  rect(c, 136, 188, 120, 10, '#8D829A', 5);
  if (variant === 'actual') {
    rect(c, 300, 206, 140, 36, '#C92A2A', 10);
    rect(c, 320, 220, 100, 8, '#FFFFFF', 4);
  } else {
    rect(c, 316, 214, 124, 32, '#7C2ACB', 10);
    rect(c, 336, 226, 84, 8, '#FFFFFF', 4);
  }
  return c;
}

function diff(a, b) {
  const c = canvas(a.w, a.h, '#FFFFFF');
  for (let i = 0; i < a.w * a.h; i++) {
    const o = i * 4;
    const same = a.px[o] === b.px[o] && a.px[o + 1] === b.px[o + 1] && a.px[o + 2] === b.px[o + 2];
    if (same) {
      const g = Math.round(255 - (255 - a.px[o]) * 0.12);
      c.px[o] = g;
      c.px[o + 1] = g;
      c.px[o + 2] = g;
    } else {
      c.px[o] = 255;
      c.px[o + 1] = 0;
      c.px[o + 2] = 0;
    }
    c.px[o + 3] = 255;
  }
  return c;
}

function screenshot() {
  const c = page('actual');
  rect(c, 120, 100, 240, 100, '#24162F', 14);
  rect(c, 140, 120, 160, 12, '#FFA8A8', 6);
  rect(c, 140, 144, 200, 10, '#B6ADC3', 5);
  rect(c, 140, 162, 170, 10, '#B6ADC3', 5);
  return c;
}

const expected = page('expected');
const actual = page('actual');
const out = {
  screenshot: png(screenshot()),
  expected: png(expected),
  actual: png(actual),
  diff: png(diff(expected, actual)),
};

const lines = Object.entries(out).map(([k, v]) => `export const ${k}Png = '${v.toString('base64')}';`);
writeFileSync(new URL('../src/mocks/images.ts', import.meta.url), `${lines.join('\n')}\n`);
for (const [k, v] of Object.entries(out)) console.log(k, v.length);
