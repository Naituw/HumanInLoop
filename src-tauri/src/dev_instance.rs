//! Dev Instance discovery and CLI entry redirection.
//!
//! When the process cwd sits under a worktree that has been `dev enable`d
//! (marker: `<root>/.askhuman-dev/enabled`), the CLI routes data and (when
//! present) re-execs into that tree's isolated binary + `ASKHUMAN_HOME`.
//! See `docs/specs/dev-instance-parallel.md`.

use std::path::{Path, PathBuf};

/// Env var: overrides `paths::config_dir()` (instance home root).
pub const ASKHUMAN_HOME_ENV: &str = "ASKHUMAN_HOME";

/// Marker file created by `dev enable` under the worktree root.
pub const ENABLED_MARKER: &str = "enabled";

/// Relative directory name under the worktree root.
pub const DEV_DIR: &str = ".askhuman-dev";

/// Whether this process is running in a Dev Instance (non-empty `ASKHUMAN_HOME`).
pub fn is_dev_instance() -> bool {
    std::env::var_os(ASKHUMAN_HOME_ENV).is_some_and(|v| !v.is_empty())
}

/// Walk upward from `start` looking for `<ancestor>/.askhuman-dev/enabled`.
/// Returns the worktree root (`ancestor`) on the nearest hit.
pub fn find_dev_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    if cur.is_file() {
        cur.pop();
    }
    loop {
        let marker = cur.join(DEV_DIR).join(ENABLED_MARKER);
        if marker.is_file() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Instance home directory for a worktree root: `<root>/.askhuman-dev/home`.
pub fn instance_home(root: &Path) -> PathBuf {
    root.join(DEV_DIR).join("home")
}

/// Instance binary path for a worktree root.
pub fn instance_bin(root: &Path) -> PathBuf {
    let name = if cfg!(windows) {
        "AskHuman.exe"
    } else {
        "AskHuman"
    };
    root.join(DEV_DIR).join("bin").join(name)
}

/// Command class for dispatcher policy (spec D18 / plan §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    /// Spawned child roles: never redirect by cwd.
    Skip,
    /// `dev` / help / version: may run without instance bin.
    Meta,
    /// Dev Instance management pins the target home but stays in the dispatcher binary. This lets
    /// Windows purge an instance without running from the executable being deleted.
    Dev,
    /// settings / history / config / channel: may run without instance bin (write instance home).
    Config,
    /// ask / daemon / mcp / …: require instance bin.
    Runtime,
}

/// Classify `argv` (full args including program name at `[0]`).
pub fn classify_command(argv: &[String]) -> CommandClass {
    let Some(first) = argv.get(1).map(|s| s.as_str()) else {
        // bare `AskHuman` with no args → treat as runtime-ish help path; no redirect needed
        // beyond env. Use Meta so missing bin does not hard-fail before help.
        return CommandClass::Meta;
    };
    match first {
        "--popup"
        | "--gui-host"
        | "__permission-diff-worker"
        | "__permission-shell-worker"
        | "__update-worker"
        | "__npm-update-worker" => CommandClass::Skip,
        "dev" => CommandClass::Dev,
        "--help" | "-h" | "--version" | "-v" | "--agent-help" | "--scripting-help" => {
            CommandClass::Meta
        }
        "--settings" | "--history" | "--todos" | "config" | "channel" => CommandClass::Config,
        _ => CommandClass::Runtime,
    }
}

/// Apply Dev Instance env and optional re-exec. Call at the very start of `cli::dispatch`.
pub fn maybe_enter_dev_instance() {
    let argv: Vec<String> = std::env::args().collect();
    if matches!(classify_command(&argv), CommandClass::Skip) {
        return;
    }

    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Some(root) = find_dev_root(&cwd) else {
        return;
    };

    let home = instance_home(&root);
    let bin = instance_bin(&root);
    let class = classify_command(&argv);

    // Data plane: always pin instance home + no main keychain for this process.
    // SAFETY: single-threaded at process entry before other threads; std::env::set_var is the
    // established pattern for CLI bootstrap in this codebase (see spawn env pass-through).
    std::env::set_var(ASKHUMAN_HOME_ENV, &home);
    std::env::set_var("ASKHUMAN_NO_KEYCHAIN", "1");

    let bin_exists = bin.is_file();
    if matches!(class, CommandClass::Runtime) && !bin_exists {
        eprintln!(
            "error: Dev Instance enabled at {} but binary is missing:\n  {}\nRun `./scripts/install.sh` in this worktree first.",
            root.display(),
            bin.display()
        );
        std::process::exit(1);
    }

    if !bin_exists {
        // Meta / Config: continue with current exe under instance env.
        return;
    }

    if matches!(class, CommandClass::Dev) {
        return;
    }

    let Ok(current) = std::env::current_exe() else {
        return;
    };
    if same_executable(&current, &bin) {
        return;
    }

    reexec_into(&bin, &home, &argv);
}

fn same_executable(a: &Path, b: &Path) -> bool {
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

fn reexec_into(bin: &Path, home: &Path, argv: &[String]) -> ! {
    let args: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(bin)
            .args(&args)
            .env(ASKHUMAN_HOME_ENV, home)
            .env("ASKHUMAN_NO_KEYCHAIN", "1")
            .exec();
        eprintln!(
            "error: failed to re-exec Dev Instance binary {}: {err}",
            bin.display()
        );
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new(bin)
            .args(&args)
            .env(ASKHUMAN_HOME_ENV, home)
            .env("ASKHUMAN_NO_KEYCHAIN", "1")
            .status();
        match status {
            Ok(s) => std::process::exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!(
                    "error: failed to re-exec Dev Instance binary {}: {e}",
                    bin.display()
                );
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn find_dev_root_nearest_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("wt");
        let nested = root.join("src").join("deep");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(root.join(DEV_DIR)).unwrap();
        fs::write(root.join(DEV_DIR).join(ENABLED_MARKER), b"").unwrap();

        assert_eq!(find_dev_root(&nested).as_deref(), Some(root.as_path()));
        assert_eq!(find_dev_root(&root).as_deref(), Some(root.as_path()));
        assert!(find_dev_root(tmp.path()).is_none());
    }

    #[test]
    fn classify_command_matrix() {
        let prog = |rest: &[&str]| {
            let mut v = vec!["AskHuman".into()];
            v.extend(rest.iter().map(|s| (*s).to_string()));
            v
        };
        assert_eq!(classify_command(&prog(&["--popup"])), CommandClass::Skip);
        assert_eq!(classify_command(&prog(&["--gui-host"])), CommandClass::Skip);
        assert_eq!(
            classify_command(&prog(&["__update-worker"])),
            CommandClass::Skip
        );
        assert_eq!(
            classify_command(&prog(&["__npm-update-worker"])),
            CommandClass::Skip
        );
        assert_eq!(
            classify_command(&prog(&["__permission-diff-worker"])),
            CommandClass::Skip
        );
        assert_eq!(
            classify_command(&prog(&["dev", "enable"])),
            CommandClass::Dev
        );
        assert_eq!(classify_command(&prog(&["--version"])), CommandClass::Meta);
        assert_eq!(
            classify_command(&prog(&["--settings"])),
            CommandClass::Config
        );
        assert_eq!(classify_command(&prog(&["--todos"])), CommandClass::Config);
        assert_eq!(
            classify_command(&prog(&["channel", "list"])),
            CommandClass::Config
        );
        assert_eq!(
            classify_command(&prog(&["daemon", "status"])),
            CommandClass::Runtime
        );
        assert_eq!(classify_command(&prog(&["hello?"])), CommandClass::Runtime);
    }
}
