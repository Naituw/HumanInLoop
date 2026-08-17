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
const windowsGeneratedDir = mkdtempSync(join(tmpdir(), "askhuman-windows-icons-"));
const windowsSource = resolve(windowsGeneratedDir, "icon-windows.png");
const bundleFiles = ["32x32.png", "128x128.png", "128x128@2x.png", "icon.icns", "icon.ico"];

function run(command, args, description) {
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    stdio: "inherit",
  });
  if (result.error) {
    throw new Error(`${description}: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`${description} exited with status ${result.status ?? "unknown"}`);
  }
}

try {
  run(process.execPath, [tauriCli, "icon", source, "--output", generatedDir], "Tauri icon generator");
  for (const name of bundleFiles) {
    copyFileSync(resolve(generatedDir, name), resolve(iconDir, name));
  }

  // Windows reserves less optical padding than macOS. Create a temporary, nearly full-bleed
  // source for the PE icon while leaving the canonical PNG and generated ICNS untouched.
  run(
    "magick",
    [
      source,
      "-trim",
      "+repage",
      "-filter",
      "Lanczos",
      "-resize",
      "248x248",
      "-gravity",
      "center",
      "-background",
      "none",
      "-extent",
      "256x256",
      `PNG32:${windowsSource}`,
    ],
    "ImageMagick Windows icon crop",
  );
  run(
    process.execPath,
    [tauriCli, "icon", windowsSource, "--output", windowsGeneratedDir],
    "Tauri Windows icon generator",
  );
  copyFileSync(resolve(windowsGeneratedDir, "icon.ico"), resolve(iconDir, "icon.ico"));

  console.log(
    `Updated ${bundleFiles.join(", ")} from src-tauri/icons/icon.png (Windows ICO uses a tight temporary crop)`,
  );
} finally {
  rmSync(generatedDir, { recursive: true, force: true });
  rmSync(windowsGeneratedDir, { recursive: true, force: true });
}
