//! 图片附件落盘：sanitize 文件名、media_type→扩展名、base64 解码。

use crate::i18n::{tr, Lang};
use crate::models::ImageAttachment;
use crate::paths;
use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::collections::HashSet;
use std::path::Path;

/// 把某个问题的图片落盘到 `temp/askhuman/<request_id>/q<question_index+1>/`，返回绝对路径列表。
///
/// 按问题划分子目录，避免多问题间 `img-1.png` 等默认文件名相互覆盖。
/// 错误文案按 `lang` 本地化。
pub fn save(
    images: &[ImageAttachment],
    request_id: &str,
    question_index: usize,
    lang: Lang,
) -> Result<Vec<String>> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let dir = paths::request_temp_dir(request_id).join(format!("q{}", question_index + 1));
    std::fs::create_dir_all(&dir).with_context(|| {
        tr(lang, "cli.createImageDirFailed").replace("{path}", &dir.display().to_string())
    })?;

    save_into_dir(images, &dir, lang)
}

fn save_into_dir(images: &[ImageAttachment], dir: &Path, lang: Lang) -> Result<Vec<String>> {
    let mut used_names = HashSet::with_capacity(images.len());
    let mut paths_out = Vec::with_capacity(images.len());
    for (index, img) in images.iter().enumerate() {
        paths_out.push(save_one(img, index, dir, &mut used_names, lang)?);
    }
    Ok(paths_out)
}

fn save_one(
    img: &ImageAttachment,
    index: usize,
    dir: &Path,
    used_names: &mut HashSet<String>,
    lang: Lang,
) -> Result<String> {
    let ext = extension_from_media_type(&img.media_type);
    let preferred_filename = match img
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(name) => sanitize_filename(name, ext),
        None => format!("img-{}.{}", index + 1, ext),
    };
    let filename = unique_filename(&preferred_filename, dir, used_names);
    let file_path = dir.join(&filename);
    let data = decode_image_data(&img.data, lang)?;
    std::fs::write(&file_path, &data).with_context(|| {
        tr(lang, "cli.writeImageFailed").replace("{path}", &file_path.display().to_string())
    })?;
    Ok(file_path.to_string_lossy().to_string())
}

/// Reserve a filename without overwriting an earlier attachment in the same answer.
/// Case-folded comparison also keeps results portable to case-insensitive filesystems.
fn unique_filename(preferred: &str, dir: &Path, used_names: &mut HashSet<String>) -> String {
    let mut candidate = preferred.to_string();
    let mut suffix = 2;
    while used_names.contains(&candidate.to_lowercase()) || dir.join(&candidate).exists() {
        candidate = filename_with_suffix(preferred, suffix);
        suffix += 1;
    }
    used_names.insert(candidate.to_lowercase());
    candidate
}

fn filename_with_suffix(filename: &str, suffix: usize) -> String {
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) if !ext.is_empty() => format!("{stem}-{suffix}.{ext}"),
        _ => format!("{stem}-{suffix}"),
    }
}

fn extension_from_media_type(media_type: &str) -> &'static str {
    match media_type.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

/// 去掉路径分隔符等危险字符，确保文件落在目标目录内。
fn sanitize_filename(raw: &str, fallback_ext: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\0'))
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        format!("img.{}", fallback_ext)
    } else {
        trimmed.to_string()
    }
}

/// 解码 base64，兼容含 `data:...;base64,` 前缀的内嵌格式。
fn decode_image_data(data: &str, lang: Lang) -> Result<Vec<u8>> {
    let payload = match data.find("base64,") {
        Some(idx) => &data[idx + "base64,".len()..],
        None => data,
    };
    let cleaned: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
    B64.decode(cleaned.as_bytes())
        .context(tr(lang, "cli.imageDecodeFailed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize_filename("/etc/passwd", "png"), "passwd");
        assert_eq!(sanitize_filename("../../foo.png", "png"), "foo.png");
        assert_eq!(sanitize_filename("a\\b\\c.jpg", "png"), "c.jpg");
        assert_eq!(sanitize_filename("", "png"), "img.png");
        assert_eq!(sanitize_filename(".....", "png"), "img.png");
    }

    #[test]
    fn extension_mapping() {
        assert_eq!(extension_from_media_type("image/png"), "png");
        assert_eq!(extension_from_media_type("image/JPEG"), "jpg");
        assert_eq!(extension_from_media_type("image/svg+xml"), "svg");
        assert_eq!(extension_from_media_type("application/unknown"), "bin");
    }

    #[test]
    fn decode_handles_data_uri() {
        let png_b64 = "iVBORw0KGgo=";
        let bytes =
            decode_image_data(&format!("data:image/png;base64,{}", png_b64), Lang::En).unwrap();
        assert_eq!(bytes, B64.decode(png_b64).unwrap());
    }

    #[test]
    fn decode_handles_whitespace() {
        let bytes = decode_image_data("aGVs\nbG8=", Lang::En).unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn duplicate_filenames_are_saved_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let images = vec![
            ImageAttachment {
                data: B64.encode(b"first image"),
                media_type: "image/png".to_string(),
                filename: Some("image.png".to_string()),
            },
            ImageAttachment {
                data: B64.encode(b"second image"),
                media_type: "image/png".to_string(),
                filename: Some("image.png".to_string()),
            },
        ];

        let saved = save_into_dir(&images, dir.path(), Lang::En).unwrap();

        assert_eq!(saved.len(), 2);
        assert_eq!(Path::new(&saved[0]).file_name().unwrap(), "image.png");
        assert_eq!(Path::new(&saved[1]).file_name().unwrap(), "image-2.png");
        assert_eq!(std::fs::read(&saved[0]).unwrap(), b"first image");
        assert_eq!(std::fs::read(&saved[1]).unwrap(), b"second image");
    }

    #[test]
    fn filename_collisions_are_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let images = vec![
            ImageAttachment {
                data: B64.encode(b"upper"),
                media_type: "image/png".to_string(),
                filename: Some("IMAGE.PNG".to_string()),
            },
            ImageAttachment {
                data: B64.encode(b"lower"),
                media_type: "image/png".to_string(),
                filename: Some("image.png".to_string()),
            },
        ];

        let saved = save_into_dir(&images, dir.path(), Lang::En).unwrap();

        assert_eq!(Path::new(&saved[0]).file_name().unwrap(), "IMAGE.PNG");
        assert_eq!(Path::new(&saved[1]).file_name().unwrap(), "image-2.png");
        assert_ne!(saved[0], saved[1]);
    }
}
