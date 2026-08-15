//! Built-in popup sounds played when a popup appears, controlled by
//! `general.popupSound`.
//!
//! - macOS: `afplay /System/Library/Sounds/<name>.aiff`; settings lists available names.
//! - Linux: best-effort freedesktop sound via `canberra-gtk-play`, `paplay`,
//!   `pw-play`, or `ogg123`; unsupported when no player is found.
//! - Windows: native system notification sound via `MessageBeep`.
//!
//! Playback is fire-and-forget: a background thread waits on the spawned player
//! process to avoid zombies.
//! `support()` returns `"named"` (macOS), `"toggle"` (Linux/Windows), or `"none"`.

/// Platform support and UI shape: `"named"` / `"toggle"` / `"none"`.
pub fn support() -> &'static str {
    imp::support()
}

/// Available sound names. Non-empty only for macOS `"named"` support.
pub fn names() -> Vec<String> {
    imp::names()
}

/// Play the selected sound. Empty string means disabled.
pub fn play(name: &str) {
    if name.trim().is_empty() {
        return;
    }
    imp::play(name);
}

/// Spawn a player and reap it in a background thread.
#[cfg(unix)]
fn spawn_player(bin: &str, args: &[std::ffi::OsString]) {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::PathBuf;

    pub fn support() -> &'static str {
        "named"
    }

    pub fn names() -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for dir in sound_dirs() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("aiff") {
                        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                            if !v.iter().any(|x| x == stem) {
                                v.push(stem.to_string());
                            }
                        }
                    }
                }
            }
        }
        v.sort();
        if v.is_empty() {
            v = DEFAULT_NAMES.iter().map(|s| s.to_string()).collect();
        }
        v
    }

    pub fn play(name: &str) {
        if let Some(path) = resolve(name) {
            super::spawn_player("afplay", &[path.into_os_string()]);
        }
    }

    fn resolve(name: &str) -> Option<PathBuf> {
        for dir in sound_dirs() {
            let p = dir.join(format!("{}.aiff", name));
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    fn sound_dirs() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/System/Library/Sounds"),
            PathBuf::from("/Library/Sounds"),
            crate::paths::home().join("Library/Sounds"),
        ]
    }

    /// Fallback names used when sound directories cannot be read.
    const DEFAULT_NAMES: &[&str] = &[
        "Basso",
        "Blow",
        "Bottle",
        "Frog",
        "Funk",
        "Glass",
        "Hero",
        "Morse",
        "Ping",
        "Pop",
        "Purr",
        "Sosumi",
        "Submarine",
        "Tink",
    ];
}

#[cfg(target_os = "linux")]
mod imp {
    use std::ffi::OsString;
    use std::path::PathBuf;

    pub fn support() -> &'static str {
        if player().is_some() {
            "toggle"
        } else {
            "none"
        }
    }

    pub fn names() -> Vec<String> {
        Vec::new()
    }

    pub fn play(_name: &str) {
        // Linux treats the setting as a toggle and ignores the concrete name.
        match player() {
            Some(Player::Canberra(bin)) => {
                super::spawn_player(&bin, &[OsString::from("-i"), OsString::from("message")])
            }
            Some(Player::File(bin, file)) => super::spawn_player(&bin, &[file]),
            None => {}
        }
    }

    enum Player {
        /// libcanberra event sound, preferred because it follows the desktop theme.
        Canberra(String),
        /// File player plus an existing freedesktop .oga sound file.
        File(String, OsString),
    }

    fn player() -> Option<Player> {
        if let Some(bin) = which("canberra-gtk-play") {
            return Some(Player::Canberra(bin));
        }
        let file = freedesktop_sound()?;
        for bin in ["paplay", "pw-play", "ogg123"] {
            if let Some(p) = which(bin) {
                return Some(Player::File(p, file));
            }
        }
        None
    }

    /// Pick an existing freedesktop notification sound file.
    fn freedesktop_sound() -> Option<OsString> {
        const CANDIDATES: [&str; 3] = [
            "/usr/share/sounds/freedesktop/stereo/message.oga",
            "/usr/share/sounds/freedesktop/stereo/complete.oga",
            "/usr/share/sounds/freedesktop/stereo/bell.oga",
        ];
        for c in CANDIDATES {
            let p = PathBuf::from(c);
            if p.is_file() {
                return Some(p.into_os_string());
            }
        }
        None
    }

    /// Find an executable in PATH and return its path.
    fn which(bin: &str) -> Option<String> {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let cand = dir.join(bin);
            if let Ok(m) = std::fs::metadata(&cand) {
                if m.is_file() && (m.permissions().mode() & 0o111 != 0) {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
        }
        None
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK;

    pub fn support() -> &'static str {
        "toggle"
    }

    pub fn names() -> Vec<String> {
        Vec::new()
    }

    pub fn play(_name: &str) {
        unsafe {
            MessageBeep(MB_ICONASTERISK);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    pub fn support() -> &'static str {
        "none"
    }
    pub fn names() -> Vec<String> {
        Vec::new()
    }
    pub fn play(_name: &str) {}
}
