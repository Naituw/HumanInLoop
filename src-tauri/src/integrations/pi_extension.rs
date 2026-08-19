//! AskHuman-owned Pi extension.
//!
//! Pi intentionally has no native hook configuration. One generated global extension carries the
//! CLI runtime, lifecycle/interjection, and Stop capabilities. The lifecycle preference lives in
//! AskHuman's shared per-agent preference file; the embedded flags describe only the extension's
//! current on-disk capability set.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const CONFIG_BEGIN: &str = "/* ASKHUMAN_CONFIG_BEGIN */";
const CONFIG_END: &str = "/* ASKHUMAN_CONFIG_END */";
const TEMPLATE: &str = include_str!("../../resources/integrations/pi/index.ts");
const TEMPLATE_CONFIG_TOKEN: &str = "__ASKHUMAN_CONFIG_JSON__";
const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ExtensionConfig {
    version: u32,
    executable: String,
    program_name: String,
    cli: bool,
    lifecycle: bool,
    stop: bool,
}

impl ExtensionConfig {
    fn empty(executable: String) -> Self {
        Self {
            version: CONFIG_VERSION,
            executable,
            program_name: crate::cli::help::program_name(),
            cli: false,
            lifecycle: false,
            stop: false,
        }
    }

    fn any_enabled(&self) -> bool {
        self.cli || self.lifecycle || self.stop
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtensionStatus {
    pub installed: bool,
    pub outdated: bool,
    pub cli: bool,
    pub lifecycle: bool,
    pub stop: bool,
}

pub fn status() -> ExtensionStatus {
    let path = crate::paths::pi_extension_file();
    let Ok(text) = std::fs::read_to_string(path) else {
        return ExtensionStatus::default();
    };
    let Some(config) = parse_config(&text) else {
        return ExtensionStatus {
            installed: true,
            outdated: true,
            ..ExtensionStatus::default()
        };
    };
    let expected = std::env::current_exe().ok().and_then(|executable| {
        render(&refreshed_config(
            config.clone(),
            executable.to_string_lossy().to_string(),
        ))
        .ok()
    });
    ExtensionStatus {
        installed: true,
        outdated: expected.as_deref() != Some(text.as_str()),
        cli: config.cli,
        lifecycle: config.lifecycle,
        stop: config.stop,
    }
}

pub fn display_path() -> String {
    collapse_home(&crate::paths::pi_extension_file())
}

pub fn set_cli_enabled(enabled: bool) -> Result<()> {
    update(|config| {
        config.cli = enabled;
        if !enabled {
            config.stop = false;
        }
    })
}

pub fn set_lifecycle_enabled(enabled: bool) -> Result<()> {
    update(|config| config.lifecycle = enabled)
}

pub fn set_stop_enabled(enabled: bool) -> Result<()> {
    update(|config| config.stop = enabled && config.cli)
}

/// Rewrite the currently selected capability set using the current executable and template.
pub fn refresh() -> Result<()> {
    update(|_| {})
}

pub fn reveal() {
    reveal_path(&crate::paths::pi_extension_file());
}

pub fn open() {
    open_path(&crate::paths::pi_extension_file());
}

fn update(change: impl FnOnce(&mut ExtensionConfig)) -> Result<()> {
    let executable = std::env::current_exe()
        .context("failed to resolve current executable")?
        .to_string_lossy()
        .to_string();
    let path = crate::paths::pi_extension_file();
    let existing = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| parse_config(&text));
    let mut config = existing.unwrap_or_else(|| ExtensionConfig::empty(executable.clone()));
    config = refreshed_config(config, executable);
    change(&mut config);

    if !config.any_enabled() {
        if path.is_file() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove Pi extension: {}", path.display()))?;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
        return Ok(());
    }

    let text = render(&config)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| {
            format!("failed to create Pi extension directory: {}", dir.display())
        })?;
    }
    atomic_write(&path, text.as_bytes())
        .with_context(|| format!("failed to write Pi extension: {}", path.display()))
}

fn render(config: &ExtensionConfig) -> Result<String> {
    let json = serde_json::to_string(config)?;
    Ok(TEMPLATE.replace(TEMPLATE_CONFIG_TOKEN, &json))
}

fn refreshed_config(mut config: ExtensionConfig, executable: String) -> ExtensionConfig {
    config.version = CONFIG_VERSION;
    config.executable = executable;
    config.program_name = crate::cli::help::program_name();
    config
}

fn parse_config(text: &str) -> Option<ExtensionConfig> {
    let start = text.find(CONFIG_BEGIN)? + CONFIG_BEGIN.len();
    let end = text[start..].find(CONFIG_END)? + start;
    serde_json::from_str(text[start..end].trim()).ok()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn collapse_home(path: &Path) -> String {
    let home = crate::paths::home();
    path.strip_prefix(home)
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn reveal_path(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-R", &path.to_string_lossy()])
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let target = path.parent().unwrap_or(path);
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.to_string_lossy()))
            .spawn();
    }
}

fn open_path(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn();
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ExtensionConfig {
        ExtensionConfig {
            version: CONFIG_VERSION,
            executable: "/Applications/AskHuman".into(),
            program_name: "AskHuman".into(),
            cli: true,
            lifecycle: true,
            stop: true,
        }
    }

    #[test]
    fn rendered_extension_roundtrips_embedded_config() {
        let config = config();
        let text = render(&config).unwrap();
        assert_eq!(parse_config(&text), Some(config));
        assert_eq!(text.matches(CONFIG_BEGIN).count(), 1);
        assert_eq!(text.matches(CONFIG_END).count(), 1);
    }

    #[test]
    fn template_contains_the_required_pi_events() {
        let text = render(&config()).unwrap();
        for event in [
            "session_start",
            "before_agent_start",
            "session_compact",
            "agent_start",
            "tool_call",
            "tool_result",
            "agent_end",
            "agent_settled",
            "session_shutdown",
        ] {
            assert!(text.contains(&format!("pi.on(\"{event}\"")), "{event}");
        }
        assert!(text.contains("input.timeout = 86400"));
        assert!(text.contains("pi.sendUserMessage"));
    }

    #[test]
    fn refreshed_config_marks_binary_path_and_template_version_current() {
        let mut stale = config();
        stale.version = 0;
        stale.executable = "/old/AskHuman".into();
        stale.program_name = "OldHuman".into();
        let refreshed = refreshed_config(stale, "/new/AskHuman".into());
        assert_eq!(refreshed.version, CONFIG_VERSION);
        assert_eq!(refreshed.executable, "/new/AskHuman");
        assert_eq!(refreshed.program_name, crate::cli::help::program_name());
        assert!(refreshed.cli && refreshed.lifecycle && refreshed.stop);
    }
}
