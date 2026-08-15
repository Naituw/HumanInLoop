#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const iconDir = resolve(repoRoot, "src-tauri/icons");
const source = resolve(iconDir, "icon.png");
const tauriCli = resolve(repoRoot, "node_modules/@tauri-apps/cli/tauri.js");
const generatedDir = mkdtempSync(join(tmpdir(), "askhuman-app-icons-"));
const bundleFiles = ["32x32.png", "128x128.png", "128x128@2x.png", "icon.icns", "icon.ico"];

try {
  const result = spawnSync(process.execPath, [tauriCli, "icon", source, "--output", generatedDir], {
    cwd: repoRoot,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Tauri icon generator exited with status ${result.status ?? "unknown"}`);
  }
  for (const name of bundleFiles) {
    copyFileSync(resolve(generatedDir, name), resolve(iconDir, name));
  }
  console.log(`Updated ${bundleFiles.join(", ")} from src-tauri/icons/icon.png`);
} finally {
  rmSync(generatedDir, { recursive: true, force: true });
}
