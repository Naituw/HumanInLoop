#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  writeFileSync,
} from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { pnpmBuildCommand } from "./package-command.mjs";

const CACHE_VERSION = 1;
const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distDir = resolve(repoRoot, "dist");
const configuredTarget = process.env.CARGO_TARGET_DIR;
const targetDir = configuredTarget
  ? resolve(repoRoot, configuredTarget)
  : resolve(repoRoot, "src-tauri/target");
const stampPath = resolve(targetDir, "frontend-build-cache.json");
const force = process.argv.includes("--force");

const inputPaths = [
  "src",
  "scripts/build-frontend-if-needed.mjs",
  "package.json",
  "pnpm-lock.yaml",
  "tsconfig.json",
  "vite.config.ts",
  ".env",
  ".env.local",
  ".env.production",
  ".env.production.local",
];

function normalizedRelative(path) {
  return relative(repoRoot, path).split(sep).join("/");
}

function collectFiles(path, files = []) {
  if (!existsSync(path)) return files;
  const stat = lstatSync(path);
  if (stat.isFile()) {
    files.push(path);
    return files;
  }
  if (!stat.isDirectory()) return files;
  for (const entry of readdirSync(path).sort()) {
    collectFiles(resolve(path, entry), files);
  }
  return files;
}

function digestFiles(paths, context = {}) {
  const hash = createHash("sha256");
  hash.update(JSON.stringify(context));
  const files = paths
    .flatMap((path) => collectFiles(path))
    .sort((a, b) => normalizedRelative(a).localeCompare(normalizedRelative(b)));
  for (const file of files) {
    hash.update("\0");
    hash.update(normalizedRelative(file));
    hash.update("\0");
    hash.update(readFileSync(file));
  }
  return { digest: hash.digest("hex"), files: files.length };
}

function readStamp() {
  try {
    return JSON.parse(readFileSync(stampPath, "utf8"));
  } catch {
    return null;
  }
}

function outputDigest() {
  if (!existsSync(resolve(distDir, "index.html"))) return null;
  return digestFiles([distDir]).digest;
}

const buildEnvironment = Object.fromEntries(
  Object.entries(process.env)
    .filter(
      ([key]) =>
        key === "ANALYZE" ||
        key === "NODE_ENV" ||
        key === "TAURI_DEV_HOST" ||
        key.startsWith("VITE_"),
    )
    .sort(([a], [b]) => a.localeCompare(b)),
);
const input = digestFiles(
  inputPaths.map((path) => resolve(repoRoot, path)),
  {
    cacheVersion: CACHE_VERSION,
    node: process.version,
    platform: process.platform,
    arch: process.arch,
    environment: buildEnvironment,
  },
);
const stamp = readStamp();
const existingOutput = outputDigest();

if (
  !force &&
  stamp?.cacheVersion === CACHE_VERSION &&
  stamp.inputDigest === input.digest &&
  stamp.outputDigest === existingOutput &&
  existingOutput
) {
  console.log(`==> 前端输入未变化，复用 dist/（${input.files} 个输入文件）`);
  process.exit(0);
}

console.log("==> 构建前端 (dist/)");
const pnpm = pnpmBuildCommand();
const result = spawnSync(pnpm.command, pnpm.args, {
  cwd: repoRoot,
  env: process.env,
  stdio: "inherit",
});
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);

const builtOutput = outputDigest();
if (!builtOutput) {
  console.error("错误: 前端构建成功但 dist/index.html 不存在");
  process.exit(1);
}

mkdirSync(dirname(stampPath), { recursive: true });
const temporaryStamp = `${stampPath}.${process.pid}.tmp`;
writeFileSync(
  temporaryStamp,
  `${JSON.stringify(
    {
      cacheVersion: CACHE_VERSION,
      inputDigest: input.digest,
      outputDigest: builtOutput,
      inputFiles: input.files,
    },
    null,
    2,
  )}\n`,
);
renameSync(temporaryStamp, stampPath);
console.log("==> 前端构建缓存已更新");
