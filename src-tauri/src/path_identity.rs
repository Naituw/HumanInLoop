//! Lexical path identity used only after callers have applied their required canonicalization.

/// Normalize separators and the platform's case semantics for stable map keys and comparisons.
pub fn key(value: &str) -> String {
    key_for(std::env::consts::OS, value)
}

pub fn equivalent(left: &str, right: &str) -> bool {
    key(left) == key(right)
}

fn key_for(platform: &str, value: &str) -> String {
    let normalized = value.replace('\\', "/");
    if platform == "windows" {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_keys_are_case_and_separator_insensitive() {
        assert_eq!(
            super::key_for("windows", r"C:\Work\Repo"),
            super::key_for("windows", "c:/work/repo")
        );
        assert_eq!(
            super::key_for("windows", r"\\Server\Share\Repo"),
            super::key_for("windows", "//server/share/repo")
        );
    }

    #[test]
    fn unix_keys_preserve_case() {
        assert_ne!(
            super::key_for("linux", "/Work/Repo"),
            super::key_for("linux", "/work/repo")
        );
    }
}
