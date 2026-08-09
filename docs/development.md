# Development

Engineering notes for contributors. User-facing docs live in [`docs/wiki/`](./wiki); design specs and plans live in [`docs/specs/`](./specs) and [`docs/plans/`](./plans). A code-level architecture overview is in [`overview.md`](./overview.md).

## Prerequisites

- [Rust toolchain](https://rustup.rs)
- [pnpm](https://pnpm.io) (Node 20+)
- macOS only: an Xcode SDK is required because some macOS-native pieces are built from Swift via `build.rs`.

## Layout

- `src/` — Vue 3 + Vite + TypeScript frontend. The Vite entry `index.html` lives here, and Vite's `root` is set to `src` (build output goes to the repo-root `dist/`, which Tauri embeds).
- `src-tauri/` — Rust backend (Tauri 2). Produces the single `AskHuman` binary.
- `scripts/` — build/install/release helpers (`install.sh`, `install-windows.ps1`, `publish.sh`, `bump-version.mjs`).
- `packaging/npm/` — npm main package (`askhuman`) and scoped per-platform binary subpackages.

## Develop, build, test

```bash
pnpm install
pnpm tauri dev                                              # Vite + Tauri debug window
pnpm build && cargo build --release \
  --manifest-path src-tauri/Cargo.toml --features custom-protocol   # release (frontend embedded at cargo build time)
cargo test --manifest-path src-tauri/Cargo.toml            # Rust unit tests
```

### UI prototyping (browser-only, no Tauri)

`src/prototype/` hosts standalone HTML entries that run in a plain browser with mock data and
hot reload — no Tauri, no daemon. They share the app's design tokens (`src/styles/*`) and
Markdown renderer, and are **excluded from release builds** (Vite only bundles `src/index.html`).

```bash
pnpm dev
# open http://localhost:5180/prototype/agent-console.html
```

Current prototypes:

- `agent-console.html` — the two-pane Agent console (spec `docs/specs/gui-agent-console.md`).
  This is the visual/interaction baseline for `views/AgentsView.vue` + `views/console/*`.

**Workflow:** iterate UI ideas on the prototype first (cheap, instant feedback, user can review
in a browser), get the design confirmed, then port to the real views. Keep the prototype in
sync when a design decision changes — it stays the reference for the next iteration round.

### Optional local git hooks (fmt + clippy)

Linux CI fails the job on `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`. To catch that before push:

```bash
./scripts/install-git-hooks.sh
```

This only installs a `pre-commit` hook (LFS hooks are left alone). It runs when staged `src-tauri/**/*.rs` files change. Typical warm cost is a few seconds; a cold clippy can take about a minute. Bypass with `git commit --no-verify`. Local toolchain may still differ slightly from CI’s stable channel.

> `--features custom-protocol` is mandatory for production builds; without it the binary runs in dev mode against the Vite dev URL and shows a blank window.

Build and install locally:

```bash
# macOS / Linux  → installs to ~/.local/bin/AskHuman
# (or <worktree>/.askhuman-dev/bin when Dev Instance is enabled — see below)
./scripts/install.sh

# Build/install the exact production release profile when needed:
./scripts/install.sh --release

# Force production install path even inside an enabled worktree:
./scripts/install.sh --global

# Windows        → installs to %LOCALAPPDATA%\Programs\AskHuman
./scripts/install-windows.ps1

# Windows exact production profile:
./scripts/install-windows.ps1 -Release
```

> Running the GUI popup on Linux needs system WebKitGTK (e.g. `libwebkit2gtk-4.1`). If it's missing and a session-based channel (Telegram / DingTalk / Feishu) is configured, AskHuman automatically uses that channel; if none is available it exits with code 3 to signal graceful degradation.

`install.sh` uses the dedicated `local-install` Cargo profile (`opt-level=0`, 64 codegen units) so
small local edits compile quickly; publishing and CI continue to use the production `release`
profile. The script fingerprints frontend inputs and reuses `dist/` when they have not changed,
preventing an unchanged Vite rebuild from invalidating Tauri's embedded resources. It also skips
copy/sign when the built and installed binary state is unchanged (local macOS signing does not
request a network timestamp). After a successful copy it enforces per-profile target budgets with
Cargo-coordinated cleanup: local package artifacts are removed first, while dependency caches are
retained whenever they fit.

Default dev/test builds retain line tables for file/line backtraces without full debugger local
variable metadata. When full source-level debugger information is needed, use:

```bash
cargo build --manifest-path src-tauri/Cargo.toml --profile full-debug
cargo test --manifest-path src-tauri/Cargo.toml --profile full-debug
```

### Parallel development (Dev Instance / git worktrees)

Multiple agents can develop in separate git worktrees without sharing one daemon or overwriting one binary. Design: [`docs/specs/dev-instance-parallel.md`](./specs/dev-instance-parallel.md). **Agent checklist:** [`docs/agent-worktree-setup.md`](./agent-worktree-setup.md).

```bash
cd /path/to/worktree
AskHuman dev enable                    # or: dev enable --preset <name>
./scripts/install.sh                   # → .askhuman-dev/bin
AskHuman "…"                           # auto re-exec into this instance
```

Default instance config is popup-only and never reads the main keychain. Optional machine-level channel presets live in `~/.askhuman/dev-presets/` with exclusive leases.

## Checklist: adding a new IM channel

There is no single `Channel` trait yet — a new channel touches many match arms. Grep an
existing channel (`slack` is the newest and most complete) and mirror every hit. Roughly:

**Rust backend**

- [ ] `src-tauri/src/<channel>/` — client crate-module: API client (`client.rs`; wrap the
  unified request exits with `track()` reporting to `channels::health`), long-connection
  router (`router.rs` / `ws.rs`), watch cards (`watch.rs`), select cards (`select.rs`),
  confirm adapter (`confirm.rs`), markdown conversion if the platform needs it.
- [ ] `src-tauri/src/channels/<channel>.rs` — ask/answer channel adapter (message + file
  delivery, card building, conversation binding), declared in `channels/mod.rs`.
- [ ] `src-tauri/src/config.rs` — `channels.<channel>` config struct + defaults.
- [ ] `src-tauri/src/secrets.rs` — keychain migration/storage for the channel's secrets
  (and update the module doc comment listing managed secrets).
- [ ] `src-tauri/src/autochannel.rs` — channel id/label, auto-activation participation.
- [ ] `src-tauri/src/daemon/unix_impl/` — `mod.rs` `ensure_<channel>_router` (report/clear
  channel health on connect), plus per-channel arms in `detect.rs`, `watch.rs`,
  `select.rs`, `inbound.rs`.
- [ ] `src-tauri/src/confirm/` — `transport.rs` / `choice_cards.rs` arms.
- [ ] `src-tauri/src/i18n.rs` — channel label + user-visible strings (en/zh).
- [ ] `src-tauri/src/cli/` — `channel_cmd.rs`, `config_cmd.rs`, `cfgio.rs`, `output.rs`,
  `help.rs` mentions; `dev_presets.rs` if dev-instance presets should cover it.

**Frontend**

- [ ] `src/lib/types.ts` — config type mirror; `src/lib/ipc.ts` if a test command exists.
- [ ] `src/views/SettingsView.vue` — channel card (enable switch, credential fields, test
  button, R7 issue banner via `channelIssueText`, "Setup guide" link via
  `CHANNEL_SETUP_DOCS`) **and** entries in the settings search index (`searchIndex`).
- [ ] `src/i18n/zh.ts` + `src/i18n/en.ts` — settings strings.

**Docs**

- [ ] `docs/wiki/<channel>-setup.md` + `.en.md`, linked from both READMEs.
- [ ] `docs/overview-configuration.md` field map; `docs/overview.md` if the repo-wide map
  changes; a spec under `docs/specs/` for non-trivial platform behaviors.

## Checklist: adding a new agent family

`AgentKind` is a closed enum matched in many places. Grep an existing kind (`grok` is the
newest) and mirror every hit. Roughly:

- [ ] `src-tauri/src/agents/mod.rs` — `AgentKind` variant + `as_str`/`label`/`parse`.
- [ ] `src-tauri/src/agents/` — per-kind logic in `detect.rs` (process-chain detection),
  `title.rs`, `transcript_full.rs`, `activity.rs`, `registry.rs`, `report.rs`, `stop.rs`.
- [ ] `src-tauri/src/agents/interject.rs` — decide whether the family supports interjection
  (grok is excluded: no reliable relay channel).
- [ ] `src-tauri/src/integrations/` — `agent_lifecycle.rs` (hook install paths),
  `agent_stop.rs`, `agent_launch.rs` (IM `/new` agent tasks), rules/skills installers.
- [ ] `src-tauri/src/prompts.rs` — interaction-protocol artifact for the family.
- [ ] `src-tauri/src/cli/` — `doctor.rs` readiness checks, `agents_cmd.rs`.
- [ ] `src-tauri/src/app/gui_host.rs` — nothing usually (labels go through
  `AgentKind::label`), but verify tray agent submenu renders the new kind.
- [ ] Frontend: `src/views/SettingsView.vue` integration tab (install docs URL in
  `AGENT_INSTALL_DOCS`, hook/rule cards), `src/i18n/*`, `src/lib/types.ts` if the kind
  appears in typed payloads.
- [ ] Docs: `docs/overview.md` agent integration section, wiki page if setup differs.

## Release

Versions across the repo are kept in sync by `scripts/bump-version.mjs` (writes `Cargo.toml`, `tauri.conf.json`, root `package.json`, the npm main package, and the platform subpackages, including the main package's lock on subpackage versions):

```bash
# 1. Bump version everywhere
node scripts/bump-version.mjs 0.2.0
git commit -am "release: v0.2.0"

# 2. Publish: verifies version consistency / not-already-published → tags → pushes (triggers CI)
./scripts/publish.sh           # add -y to skip the confirmation prompt
```

`scripts/publish.sh` checks that all versions match and that the version isn't already on npm (errors and asks you to bump otherwise), then tags and pushes. `.github/workflows/release.yml` then compiles the four platform binaries → publishes to npm (main package + platform subpackages) → creates a GitHub Release.

> Prerequisite: set `NPM_TOKEN` (an npmjs automation token) under the repo's Settings → Secrets. Pre-release versions (e.g. `0.2.0-rc.1`) are published under the npm dist-tag `next` and marked as a GitHub pre-release.

The release architecture and channel-degradation design are documented in [`docs/plans/release-and-channel-degradation.md`](./plans/release-and-channel-degradation.md).
