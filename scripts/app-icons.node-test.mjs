import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const iconDir = resolve(repoRoot, "src-tauri/icons");

function pngSize(name) {
  const bytes = readFileSync(resolve(iconDir, name));
  assert.equal(bytes.subarray(1, 4).toString("ascii"), "PNG");
  return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
}

test("application PNG bundle contains the required generated sizes", () => {
  assert.deepEqual(pngSize("icon.png"), [256, 256]);
  assert.deepEqual(pngSize("32x32.png"), [32, 32]);
  assert.deepEqual(pngSize("128x128.png"), [128, 128]);
  assert.deepEqual(pngSize("128x128@2x.png"), [256, 256]);
});

test("Windows ICO contains every Tauri desktop resolution", () => {
  const bytes = readFileSync(resolve(iconDir, "icon.ico"));
  assert.equal(bytes.readUInt16LE(0), 0);
  assert.equal(bytes.readUInt16LE(2), 1);
  const count = bytes.readUInt16LE(4);
  const sizes = Array.from({ length: count }, (_, index) => {
    const offset = 6 + index * 16;
    return bytes[offset] || 256;
  });
  assert.deepEqual(sizes.sort((a, b) => a - b), [16, 24, 32, 48, 64, 256]);
});
