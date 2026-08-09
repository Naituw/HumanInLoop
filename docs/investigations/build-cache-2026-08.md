# Build cache and incremental install investigation (2026-08)

## Outcome

The repository had reached roughly 15–20 GiB while a normal warm `install.sh` took about
205 seconds. The two symptoms had the same underlying amplifier: every install rebuilt `dist/`,
which invalidated Tauri's embedded frontend resources and forced the large Rust binary crate
through an almost-cold release rebuild.

The implemented development path now:

- fingerprints frontend inputs and preserves `dist/` when they are unchanged;
- builds local installs with the `local-install` profile (`opt-level=0`, 64 codegen units);
- keeps the production `release` profile unchanged (`opt-level="z"`, 16 codegen units);
- skips copy/sign when both the built source binary and installed binary match saved hashes;
- keeps Swift/Clang module caches inside Cargo's `OUT_DIR`;
- uses Cargo-coordinated per-profile budgets instead of deleting incremental directories directly;
- uses line-table debug information by default, with `full-debug` available explicitly.

## Before: measured state

The main worktree was the only live project worktree with build output. Its target breakdown was:

| Directory | Size | Main contents |
| --- | ---: | --- |
| `target/debug` | 11 GiB | tests, Clippy, dev incremental state |
| `target/debug/deps` | 9.5 GiB | dependency artifacts and Rust codegen objects |
| `target/release` | 2.3 GiB | local install/release dependencies and incremental state |
| Entire repository | about 15 GiB | almost entirely `src-tauri/target` |

`debug/deps` contained 15,395 `AskHuman-*.rcgu.o` files across seven recent build hashes,
with a logical size of about 10.7 GiB. The old 14-day `cargo-sweep` window allowed active
development to create many generations before any became eligible. The custom incremental
pruner deleted raw directories without Cargo's target lock; many objects were hard-linked from
`deps`, so removing only the incremental path often did not release the blocks.

## Compile benchmarks

Environment: Apple Silicon macOS, Rust/Cargo 1.94.1, Node 22.14, pnpm 10.13.1. Cold targets reused
already-downloaded crates but no compiled artifacts.

### Original path

| Scenario | Wall time |
| --- | ---: |
| Frontend dependency check | 0.423 s |
| Frontend production build | 4.38 s |
| Production release, cold target | 215.39 s |
| Cargo no-op without rewriting `dist` | 0.88–1.29 s |
| One real small Rust edit, without rebuilding frontend | 30.34 s |
| Unchanged frontend rebuilt first, then Cargo | 200.67 s |
| Estimated old warm install compile path | about 205.5 s plus copy/sign/cleanup |

A cold release target was 1.9 GiB. One small Rust edit grew it to 2.2 GiB; release incremental
state grew from 480 MiB to 806 MiB, showing why repeated forced rebuilds were both slow and large.

### Profile experiment

Each incremental measurement changed the same leaf help string after a cold build.

| Profile override | Cold build | Small edit | Incremental growth | Binary |
| --- | ---: | ---: | ---: | ---: |
| `z`, 16 CGU (original) | 215.39 s | 30.34 s | about 326 MiB | about 14 MiB |
| `z`, 64 CGU | 210.79 s | 20.70 s | about 357 MiB | 14.3 MiB |
| opt 2, 64 CGU | 226.26 s | 32.39 s | about 332 MiB | 20.5 MiB |
| opt 1, 64 CGU | 200.56 s | 22.86 s | about 359 MiB | 20.4 MiB |
| opt 1, 128 CGU | 208.03 s | 26.96 s | about 354 MiB | 20.5 MiB |
| opt 0, 64 CGU | 122.08 s | 11.26 s | about 333 MiB | 38.1 MiB |

For opt 0 / 64 CGU, rebuilding only the local package after retaining dependencies took 36.53
seconds. Ten `--version` launches took 1.57 seconds versus 0.73 seconds for the production
binary, an approximately 84 ms per-process development penalty. This trade-off applies only to
locally installed development binaries; release publishing remains fully optimized.

### Implemented end-to-end path

| Scenario | Wall time |
| --- | ---: |
| Stable no-change `install.sh` | 2.26 s |
| One-line Rust edit, full install | 11.62 s / 12.77 s (forward/revert) |
| One-line frontend edit, full install | 19.22 s / 16.51 s (forward/revert) |

These numbers include pnpm's dependency check, frontend fingerprint/build as applicable, Cargo,
copy, formal local signing, and cache checks.

## Cache policy

After every successful copy, the installer checks actual profile directory sizes:

| Profile | Budget |
| --- | ---: |
| `local-install` | 4 GiB |
| `dev` / `test` (`target/debug`) | 6 GiB |
| `full-debug` | 6 GiB |
| production `release` | 4 GiB |

When a profile exceeds its budget, `cargo clean -p humaninloop --profile <name>` removes only this
workspace package first, retaining third-party dependencies. If the remaining dependencies alone
still exceed the budget, the installer cleans that whole profile. Cargo owns the target lock for
both operations. Optional `cargo-sweep --time 7` separately retires unused dependency hashes;
budget enforcement does not depend on cargo-sweep being installed.

The first real install after this change reclaimed 5.64 GiB through cargo-sweep and reduced the
debug profile from 7,534 MiB to 3,135 MiB through package-only cleanup. After rebuilding the new
debug/test profile and running the full test suite, target remained around 11 GiB across active
debug, local-install, and legacy release caches. A one-time package-only cleanup of the superseded
release install artifacts then reclaimed another 1.9 GiB while retaining release dependencies,
leaving `target` at 8.9 GiB and the whole repository at about 10 GiB.

## When to revisit architecture

The Rust source grew from 35,683 lines on 2026-06-19 to 109,348 lines on 2026-08-09 while remaining
one binary crate. Module files do not create Cargo compilation boundaries. If small local-install
edits again exceed roughly 15 seconds despite an unchanged frontend, the next investigation should
measure extracting stable subsystems or Tauri resource context into separate crates. Profile
tuning has already been measured; repeating it without a material compiler/project change is
unlikely to help.
