export function pnpmBuildCommand({
  platform = process.platform,
  env = process.env,
} = {}) {
  if (platform === "win32") {
    return {
      command: env.ComSpec || env.COMSPEC || "cmd.exe",
      args: ["/d", "/s", "/c", "pnpm.cmd build"],
    };
  }

  return { command: "pnpm", args: ["build"] };
}
