import assert from "node:assert/strict";
import test from "node:test";

import { pnpmBuildCommand } from "./package-command.mjs";

test("uses pnpm directly outside Windows", () => {
  assert.deepEqual(pnpmBuildCommand({ platform: "linux", env: {} }), {
    command: "pnpm",
    args: ["build"],
  });
});

test("runs the Windows command shim through cmd.exe", () => {
  assert.deepEqual(
    pnpmBuildCommand({
      platform: "win32",
      env: { ComSpec: "C:\\Windows\\System32\\cmd.exe" },
    }),
    {
      command: "C:\\Windows\\System32\\cmd.exe",
      args: ["/d", "/s", "/c", "pnpm.cmd build"],
    },
  );
});

test("falls back to cmd.exe when ComSpec is absent", () => {
  assert.equal(
    pnpmBuildCommand({ platform: "win32", env: {} }).command,
    "cmd.exe",
  );
});
