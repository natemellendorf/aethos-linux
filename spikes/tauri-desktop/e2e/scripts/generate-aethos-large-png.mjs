import fs from "node:fs/promises";
import path from "node:path";
import crypto from "node:crypto";
import { PNG } from "pngjs";

function parseArgs(argv) {
  const out = argv[argv.indexOf("--out") + 1] || "";
  const seed = argv[argv.indexOf("--seed") + 1] || "aethos";
  const width = Number(argv[argv.indexOf("--width") + 1] || "2200");
  const height = Number(argv[argv.indexOf("--height") + 1] || "1600");
  if (!out) throw new Error("--out is required");
  return {
    out,
    seed,
    width: Math.max(256, Math.min(4096, Math.floor(width))),
    height: Math.max(256, Math.min(4096, Math.floor(height)))
  };
}

function deterministicByte(seedBuf, x, y, channel) {
  const hash = crypto.createHash("sha256");
  hash.update(seedBuf);
  hash.update(Buffer.from([channel & 0xff]));
  const coord = Buffer.allocUnsafe(8);
  coord.writeUInt32LE(x >>> 0, 0);
  coord.writeUInt32LE(y >>> 0, 4);
  hash.update(coord);
  return hash.digest()[0];
}

function glyph5x7(ch) {
  const map = {
    A: [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
    E: [0x1f, 0x10, 0x1e, 0x10, 0x10, 0x10, 0x1f],
    H: [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11],
    O: [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e],
    S: [0x0f, 0x10, 0x0e, 0x01, 0x01, 0x11, 0x0e],
    T: [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
    "-": [0, 0, 0, 0x1f, 0, 0, 0],
    _: [0, 0, 0, 0, 0, 0, 0x1f],
    " ": [0, 0, 0, 0, 0, 0, 0]
  };
  if (map[ch]) return map[ch];
  if (/^[0-9]$/.test(ch)) {
    const digits = {
      0: [0x0e, 0x13, 0x15, 0x19, 0x11, 0x11, 0x0e],
      1: [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
      2: [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
      3: [0x1e, 0x01, 0x06, 0x01, 0x01, 0x11, 0x0e],
      4: [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
      5: [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
      6: [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
      7: [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
      8: [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
      9: [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x1c]
    };
    return digits[ch];
  }
  return [0x1f, 0x01, 0x06, 0x04, 0x00, 0x04, 0x04];
}

function drawCaption(png, seedBuf, text) {
  const scale = Math.max(2, Math.floor(Math.min(png.width, png.height) / 256));
  let x = Math.max(8, Math.floor(png.width / 12));
  const y0 = Math.max(8, png.height - 12 * scale);
  for (let i = 0; i < text.length; i += 1) {
    const glyph = glyph5x7(text[i]);
    const jitter = (seedBuf[i % seedBuf.length] % 5) - 2;
    for (let row = 0; row < 7; row += 1) {
      for (let col = 0; col < 5; col += 1) {
        if (((glyph[row] >> (4 - col)) & 1) === 0) continue;
        for (let oy = 0; oy < scale; oy += 1) {
          for (let ox = 0; ox < scale; ox += 1) {
            const px = x + col * scale + ox + jitter;
            const py = y0 + row * scale + oy;
            if (px < 0 || py < 0 || px >= png.width || py >= png.height) continue;
            const idx = (py * png.width + px) << 2;
            png.data[idx] = 233;
            png.data[idx + 1] = 246;
            png.data[idx + 2] = 255;
            png.data[idx + 3] = 255;
          }
        }
      }
    }
    x += 6 * scale;
    if (x >= png.width - 6 * scale) break;
  }
}

async function main() {
  const { out, seed, width, height } = parseArgs(process.argv);
  const seedBuf = crypto.createHash("sha256").update(seed, "utf8").digest();
  const png = new PNG({ width, height });
  const palette = [
    [12, 21, 48],
    [34, 77, 180],
    [18, 139, 188],
    [137, 88, 212],
    [222, 241, 255]
  ];

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const idx = (y * width + x) << 2;
      const t = deterministicByte(seedBuf, x, y, 0);
      const base = palette[t % palette.length];
      const noise = deterministicByte(seedBuf, x, y, 1);
      const glow = Math.floor((x / Math.max(1, width)) * 255);
      png.data[idx] = Math.min(255, base[0] + Math.floor(noise / 3) + Math.floor(glow / 6));
      png.data[idx + 1] = Math.min(255, base[1] + Math.floor(noise / 4) + Math.floor(glow / 8));
      png.data[idx + 2] = Math.min(255, base[2] + Math.floor(noise / 5) + Math.floor(glow / 10));
      png.data[idx + 3] = 255;
    }
  }

  const label = `AETHOS ${String(seed).replace(/[^a-zA-Z0-9-_ ]/g, " ").toUpperCase()}`;
  drawCaption(png, seedBuf, label);

  const output = PNG.sync.write(png, { colorType: 6, inputColorType: 6, filterType: 4 });
  await fs.mkdir(path.dirname(out), { recursive: true });
  await fs.writeFile(out, output);
  const digest = crypto.createHash("sha256").update(output).digest("hex");
  process.stdout.write(
    `${JSON.stringify({ filePath: out, sizeBytes: output.length, sha256Hex: digest, width, height })}\n`
  );
}

main().catch((error) => {
  process.stderr.write(`${String(error?.stack || error)}\n`);
  process.exitCode = 1;
});
