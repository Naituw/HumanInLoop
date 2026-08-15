//! Machine-level Dev Instance channel presets + exclusive leases.
//!
//! Layout (never loaded by the production daemon):
//!   ~/.askhuman/dev-presets/index.json
//!   ~/.askhuman/dev-presets/<name>.json
//!
//! See `docs/specs/dev-instance-parallel.md` §7.2.

use crate::config::{AppConfig, ChannelsConfig};
use crate::dev_instance::{self, DEV_DIR, ENABLED_MARKER};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const INDEX_FILE: &str = "index.json";
const META_FILE: &str = "dev-meta.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PresetIndex {
    pub presets: BTreeMap<String, PresetIndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetIndexEntry {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<PresetLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetLease {
    pub worktree_root: String,
    pub claimed_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DevMeta {
    pub applied_presets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetBody {
    /// Channel fragment materialised into instance config.
    pub channels: ChannelsConfig,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn presets_dir() -> PathBuf {
    paths::dev_presets_dir()
}

fn index_path() -> PathBuf {
    presets_dir().join(INDEX_FILE)
}

fn lock_path() -> PathBuf {
    presets_dir().join("presets.lock")
}

fn preset_file_path(name: &str) -> PathBuf {
    presets_dir().join(format!("{name}.json"))
}

fn meta_path(home: &Path) -> PathBuf {
    home.join(META_FILE)
}

fn ensure_presets_dir() -> std::io::Result<()> {
    let dir = presets_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn lock_presets() -> Option<crate::file_lock::FileLock> {
    let _ = ensure_presets_dir();
    crate::file_lock::FileLock::exclusive(&lock_path()).ok()
}

fn read_index() -> PresetIndex {
    let path = index_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => PresetIndex::default(),
    }
}

fn write_index(index: &PresetIndex) -> Result<(), String> {
    ensure_presets_dir().map_err(|e| e.to_string())?;
    let path = index_path();
    let data = serde_json::to_vec_pretty(index).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

fn lease_is_live(lease: &PresetLease) -> bool {
    let root = PathBuf::from(&lease.worktree_root);
    root.join(DEV_DIR).join(ENABLED_MARKER).is_file()
}

/// Sanitize preset name: letters, digits, `-`, `_`, `.` only.
pub fn validate_preset_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("preset name must be 1..=64 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("preset name may only contain letters, digits, '-', '_', '.'".into());
    }
    Ok(())
}

/// Which IM channels in `channels` look configured (enabled or key fields set).
pub fn configured_channel_ids(channels: &ChannelsConfig) -> Vec<&'static str> {
    let mut out = Vec::new();
    if channels.telegram.enabled
        || !channels.telegram.bot_token.is_empty()
        || !channels.telegram.chat_id.is_empty()
    {
        out.push("telegram");
    }
    if channels.dingding.enabled
        || !channels.dingding.client_id.is_empty()
        || !channels.dingding.client_secret.is_empty()
    {
        out.push("dingding");
    }
    if channels.feishu.enabled
        || !channels.feishu.app_id.is_empty()
        || !channels.feishu.app_secret.is_empty()
    {
        out.push("feishu");
    }
    if channels.slack.enabled
        || !channels.slack.bot_token.is_empty()
        || !channels.slack.app_token.is_empty()
    {
        out.push("slack");
    }
    out
}

/// Build a channels fragment that keeps only configured IM channels (popup left default).
pub fn extract_configured_channels(cfg: &AppConfig) -> Result<ChannelsConfig, String> {
    let ids = configured_channel_ids(&cfg.channels);
    if ids.is_empty() {
        return Err(
            "no IM channels configured in this instance; open settings or use `channel set` first"
                .into(),
        );
    }
    let mut out = ChannelsConfig::default();
    // Keep popup defaults; only copy IM slices that are configured.
    if ids.contains(&"telegram") {
        out.telegram = cfg.channels.telegram.clone();
    }
    if ids.contains(&"dingding") {
        out.dingding = cfg.channels.dingding.clone();
    }
    if ids.contains(&"feishu") {
        out.feishu = cfg.channels.feishu.clone();
    }
    if ids.contains(&"slack") {
        out.slack = cfg.channels.slack.clone();
    }
    Ok(out)
}

fn channel_overlap(a: &ChannelsConfig, b: &ChannelsConfig) -> Vec<&'static str> {
    let mut out = Vec::new();
    for id in configured_channel_ids(a) {
        if configured_channel_ids(b).contains(&id) {
            out.push(id);
        }
    }
    out
}

fn merge_channels(dst: &mut ChannelsConfig, src: &ChannelsConfig) {
    for id in configured_channel_ids(src) {
        match id {
            "telegram" => dst.telegram = src.telegram.clone(),
            "dingding" => dst.dingding = src.dingding.clone(),
            "feishu" => dst.feishu = src.feishu.clone(),
            "slack" => dst.slack = src.slack.clone(),
            _ => {}
        }
    }
}

fn load_preset_body(name: &str) -> Result<PresetBody, String> {
    let path = preset_file_path(name);
    let bytes = std::fs::read(&path).map_err(|e| format!("preset '{name}' not found: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("preset '{name}' corrupt: {e}"))
}

fn save_preset_body(name: &str, body: &PresetBody) -> Result<(), String> {
    ensure_presets_dir().map_err(|e| e.to_string())?;
    let path = preset_file_path(name);
    let data = serde_json::to_vec_pretty(body).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn read_meta(home: &Path) -> DevMeta {
    match std::fs::read(meta_path(home)) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
        Err(_) => DevMeta::default(),
    }
}

fn write_meta(home: &Path, meta: &DevMeta) -> Result<(), String> {
    let path = meta_path(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_vec_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(&path, data).map_err(|e| e.to_string())
}

/// Save preset from a channels fragment (from-instance or constructed).
pub fn save_preset(name: &str, channels: ChannelsConfig) -> Result<(), String> {
    validate_preset_name(name)?;
    let _lock = lock_presets();
    save_preset_body(name, &PresetBody { channels })?;
    let mut index = read_index();
    index
        .presets
        .entry(name.to_string())
        .or_insert_with(|| PresetIndexEntry {
            file: format!("{name}.json"),
            lease: None,
        })
        .file = format!("{name}.json");
    write_index(&index)?;
    Ok(())
}

pub fn list_presets() -> Vec<(String, Option<PresetLease>, Vec<&'static str>)> {
    let _lock = lock_presets();
    let index = read_index();
    let mut out = Vec::new();
    for (name, entry) in index.presets {
        let channels = load_preset_body(&name)
            .map(|b| configured_channel_ids(&b.channels))
            .unwrap_or_default();
        let lease = entry.lease.filter(lease_is_live);
        out.push((name, lease, channels));
    }
    out
}

pub fn show_preset(name: &str) -> Result<(PresetBody, Option<PresetLease>), String> {
    let _lock = lock_presets();
    let body = load_preset_body(name)?;
    let index = read_index();
    let lease = index
        .presets
        .get(name)
        .and_then(|e| e.lease.clone())
        .filter(lease_is_live);
    Ok((body, lease))
}

pub fn release_lease(name: &str) -> Result<(), String> {
    validate_preset_name(name)?;
    let _lock = lock_presets();
    let mut index = read_index();
    if let Some(entry) = index.presets.get_mut(name) {
        entry.lease = None;
        write_index(&index)?;
        Ok(())
    } else {
        Err(format!("preset '{name}' not found"))
    }
}

pub fn remove_preset(name: &str, force: bool) -> Result<(), String> {
    validate_preset_name(name)?;
    let _lock = lock_presets();
    let mut index = read_index();
    let Some(entry) = index.presets.get(name) else {
        return Err(format!("preset '{name}' not found"));
    };
    if let Some(lease) = &entry.lease {
        if lease_is_live(lease) && !force {
            return Err(format!(
                "preset '{name}' is leased by {}; use --force or release first",
                lease.worktree_root
            ));
        }
    }
    index.presets.remove(name);
    write_index(&index)?;
    let _ = std::fs::remove_file(preset_file_path(name));
    Ok(())
}

/// Claim presets for `worktree_root` and materialise into instance home config.
pub fn apply_presets_to_instance(
    worktree_root: &Path,
    home: &Path,
    preset_names: &[String],
    force: bool,
) -> Result<Vec<String>, String> {
    if preset_names.is_empty() {
        return Ok(Vec::new());
    }
    for n in preset_names {
        validate_preset_name(n)?;
    }

    let _lock = lock_presets();
    let mut index = read_index();

    // Load bodies and check channel overlap across selected presets.
    let mut merged = ChannelsConfig::default();
    let mut bodies: Vec<(String, PresetBody)> = Vec::new();
    for name in preset_names {
        let body = load_preset_body(name)?;
        let overlap = channel_overlap(&merged, &body.channels);
        if !overlap.is_empty() {
            return Err(format!(
                "channel conflict across presets: {} (same channel in multiple --preset)",
                overlap.join(", ")
            ));
        }
        merge_channels(&mut merged, &body.channels);
        bodies.push((name.clone(), body));
    }

    let root_str = worktree_root
        .canonicalize()
        .unwrap_or_else(|_| worktree_root.to_path_buf())
        .to_string_lossy()
        .into_owned();

    for name in preset_names {
        let entry = index
            .presets
            .entry(name.clone())
            .or_insert_with(|| PresetIndexEntry {
                file: format!("{name}.json"),
                lease: None,
            });
        // Ensure body file is registered.
        entry.file = format!("{name}.json");
        if let Some(lease) = &entry.lease {
            if lease_is_live(lease) {
                let holder = PathBuf::from(&lease.worktree_root);
                let holder_canon = holder
                    .canonicalize()
                    .unwrap_or(holder)
                    .to_string_lossy()
                    .into_owned();
                if holder_canon != root_str {
                    if !force {
                        return Err(format!(
                            "preset '{name}' is already used by worktree:\n  {holder_canon}\nDisable there (`AskHuman dev disable`) or re-run with --force to steal the lease."
                        ));
                    }
                    eprintln!(
                        "warning: stealing preset '{name}' lease from {holder_canon}; if that instance daemon is still running it may double-connect the bot"
                    );
                }
            }
        }
        entry.lease = Some(PresetLease {
            worktree_root: root_str.clone(),
            claimed_at: now_secs(),
        });
    }
    write_index(&index)?;

    // Materialise into instance config.
    let cfg_path = home.join("config.json");
    let mut cfg = if cfg_path.exists() {
        AppConfig::load_from(&cfg_path)
    } else {
        AppConfig::default()
    };
    merge_channels(&mut cfg.channels, &merged);
    // Instance mode: secrets stay plaintext in config (no keychain).
    cfg.save_to(&cfg_path).map_err(|e| e.to_string())?;

    let mut meta = read_meta(home);
    for name in preset_names {
        if !meta.applied_presets.iter().any(|p| p == name) {
            meta.applied_presets.push(name.clone());
        }
    }
    write_meta(home, &meta)?;

    let _ = bodies;
    Ok(preset_names.to_vec())
}

/// Release any leases held by this worktree (on disable).
pub fn release_leases_for_worktree(worktree_root: &Path) -> Result<Vec<String>, String> {
    let _lock = lock_presets();
    let root_str = worktree_root
        .canonicalize()
        .unwrap_or_else(|_| worktree_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let mut index = read_index();
    let mut released = Vec::new();
    for (name, entry) in index.presets.iter_mut() {
        if let Some(lease) = &entry.lease {
            let holder = PathBuf::from(&lease.worktree_root);
            let holder_canon = holder
                .canonicalize()
                .unwrap_or(holder)
                .to_string_lossy()
                .into_owned();
            if holder_canon == root_str {
                entry.lease = None;
                released.push(name.clone());
            }
        }
    }
    if !released.is_empty() {
        write_index(&index)?;
    }
    // Clear meta if present.
    let home = dev_instance::instance_home(worktree_root);
    if home.exists() {
        let mut meta = read_meta(&home);
        meta.applied_presets.clear();
        let _ = write_meta(&home, &meta);
    }
    Ok(released)
}

/// Redact secrets for display.
pub fn redact_channels(channels: &ChannelsConfig) -> serde_json::Value {
    let mut v = serde_json::to_value(channels).unwrap_or(serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        for key in ["telegram", "dingding", "feishu", "slack"] {
            if let Some(ch) = obj.get_mut(key).and_then(|x| x.as_object_mut()) {
                for secret_key in ["botToken", "clientSecret", "appSecret", "appToken"] {
                    if let Some(val) = ch.get(secret_key).and_then(|x| x.as_str()) {
                        if !val.is_empty() {
                            ch.insert(secret_key.to_string(), serde_json::json!("***"));
                        }
                    }
                }
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FeishuChannelConfig;

    #[test]
    fn validate_name() {
        assert!(validate_preset_name("feishu-test").is_ok());
        assert!(validate_preset_name("").is_err());
        assert!(validate_preset_name("a/b").is_err());
    }

    #[test]
    fn configured_ids() {
        let mut ch = ChannelsConfig::default();
        assert!(configured_channel_ids(&ch).is_empty());
        ch.feishu = FeishuChannelConfig {
            enabled: true,
            app_id: "cli_x".into(),
            app_secret: "s".into(),
            open_id: "ou".into(),
            ..Default::default()
        };
        assert_eq!(configured_channel_ids(&ch), vec!["feishu"]);
    }
}
