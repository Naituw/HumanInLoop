//! `-f` 文件附件的路径解析与校验：`~` 展开、相对 CWD 解析、存在性校验、元数据采集。

use crate::models::FileAttachment;
use crate::paths;
use std::path::{Path, PathBuf};

/// 图片类扩展名（小写，无点）。
const IMAGE_EXTS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"];

/// 把命令行给出的原始路径列表解析成 `FileAttachment`。
/// 任一文件不存在/不可访问 → 返回按 `lang` 本地化的错误（调用方据此退出码 1）。
pub fn resolve(
    raw_paths: &[String],
    lang: crate::i18n::Lang,
) -> Result<Vec<FileAttachment>, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = paths::home();
    let mut out = Vec::with_capacity(raw_paths.len());
    for raw in raw_paths {
        out.push(resolve_one(raw, &cwd, &home, lang)?);
    }
    Ok(out)
}

fn resolve_one(
    raw: &str,
    cwd: &Path,
    home: &Path,
    lang: crate::i18n::Lang,
) -> Result<FileAttachment, String> {
    use crate::i18n::tr;
    let expanded = expand_tilde(raw, home);
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };

    let meta =
        std::fs::metadata(&abs).map_err(|_| tr(lang, "cli.fileNotFound").replace("{path}", raw))?;
    if !meta.is_file() {
        return Err(tr(lang, "cli.notAFile").replace("{path}", raw));
    }

    // 规整为绝对路径（去掉 ./ ../ 等）；失败则退回拼接结果。
    let canonical = std::fs::canonicalize(&abs).unwrap_or(abs);
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string());

    Ok(FileAttachment {
        path: canonical.to_string_lossy().into_owned(),
        name,
        size: meta.len(),
        is_image: is_image_ext(&canonical.to_string_lossy()),
    })
}

/// `~`, `~/...`, or `~\...` expands to the home directory; other paths are unchanged.
pub fn expand_tilde(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// 按扩展名判定是否为图片类。
pub fn is_image_ext(name: &str) -> bool {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some(ext) => IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_to_home() {
        let home = PathBuf::from("/Users/test");
        assert_eq!(expand_tilde("~", &home), home);
        assert_eq!(
            expand_tilde("~/Documents/a.md", &home),
            home.join("Documents/a.md")
        );
        assert_eq!(
            expand_tilde("~\\Documents\\a.md", &home),
            home.join("Documents\\a.md")
        );
    }

    #[test]
    fn tilde_only_at_start() {
        let home = PathBuf::from("/Users/test");
        assert_eq!(expand_tilde("/abs/~/x", &home), PathBuf::from("/abs/~/x"));
        assert_eq!(expand_tilde("rel/path", &home), PathBuf::from("rel/path"));
    }

    #[test]
    fn image_extension_detection() {
        assert!(is_image_ext("a.png"));
        assert!(is_image_ext("/x/y/Photo.JPG"));
        assert!(is_image_ext("z.jpeg"));
        assert!(is_image_ext("icon.svg"));
        assert!(!is_image_ext("doc.md"));
        assert!(!is_image_ext("archive.tar.gz"));
        assert!(!is_image_ext("noext"));
    }

    #[test]
    fn resolve_reports_missing_file() {
        let res = resolve(
            &["/definitely/not/here-xyz.md".to_string()],
            crate::i18n::Lang::En,
        );
        assert!(res.is_err());
    }

    #[test]
    fn resolve_collects_metadata() {
        let dir = std::env::temp_dir();
        let file = dir.join("humaninloop_test_attachment.txt");
        std::fs::write(&file, b"hello world").unwrap();
        let res = resolve(
            &[file.to_string_lossy().into_owned()],
            crate::i18n::Lang::En,
        )
        .unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "humaninloop_test_attachment.txt");
        assert_eq!(res[0].size, 11);
        assert!(!res[0].is_image);
        let _ = std::fs::remove_file(&file);
    }
}
