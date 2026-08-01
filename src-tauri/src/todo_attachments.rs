//! Managed storage, thumbnail caches, and Agent delivery copies for todo attachments.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MANAGED_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_ATTACHMENTS_PER_TODO: usize = 20;
pub const THUMBNAIL_MAX_EDGE: u32 = 128;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const MAX_THUMBNAIL_PIXELS: u64 = 100_000_000;
const ORPHAN_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TodoAttachmentStorage {
    Managed,
    Reference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoAttachment {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub is_image: bool,
    pub source_path: String,
    pub path: String,
    pub storage: TodoAttachmentStorage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_modified_ms: Option<u64>,
}

impl TodoAttachment {
    pub fn snapshot(&self) -> TodoAttachmentSnapshot {
        TodoAttachmentSnapshot {
            id: self.id.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            source_path: self.source_path.clone(),
            storage: self.storage,
        }
    }

    pub fn available(&self) -> bool {
        Path::new(&self.path).is_file()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoAttachmentSnapshot {
    pub id: String,
    pub name: String,
    pub path: String,
    pub source_path: String,
    pub storage: TodoAttachmentStorage,
}

#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error("invalid todo id")]
    InvalidTodoId,
    #[error("attachment path is empty or contains unsupported control characters: {0}")]
    InvalidPath(String),
    #[error("attachment does not exist or is not a readable regular file: {0}")]
    NotAFile(String),
    #[error("a todo can have at most {MAX_ATTACHMENTS_PER_TODO} attachments")]
    TooMany,
    #[error("failed to prepare attachment {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct StagedAttachmentBatch {
    pub attachments: Vec<TodoAttachment>,
    staged: Vec<(PathBuf, PathBuf)>,
    stage_dir: PathBuf,
    storage_root: PathBuf,
    committed: Vec<PathBuf>,
}

impl StagedAttachmentBatch {
    pub fn empty() -> Self {
        Self {
            attachments: Vec::new(),
            staged: Vec::new(),
            stage_dir: PathBuf::new(),
            storage_root: PathBuf::new(),
            committed: Vec::new(),
        }
    }

    /// Move prepared files into their final managed locations before the JSON transaction.
    pub fn commit_files(&mut self) -> Result<(), AttachmentError> {
        for index in 0..self.staged.len() {
            let (staged, final_path) = self.staged[index].clone();
            let moved = (|| {
                if let Some(parent) = final_path.parent() {
                    fs::create_dir_all(parent).map_err(|source| AttachmentError::Io {
                        path: parent.display().to_string(),
                        source,
                    })?;
                    harden_dir(parent);
                }
                if validated_managed_file_at(
                    &self.storage_root,
                    None,
                    &final_path.to_string_lossy(),
                    None,
                )
                .is_none()
                {
                    return Err(AttachmentError::InvalidPath(
                        final_path.display().to_string(),
                    ));
                }
                fs::rename(&staged, &final_path).map_err(|source| AttachmentError::Io {
                    path: final_path.display().to_string(),
                    source,
                })?;
                harden_file(&final_path);
                Ok::<(), AttachmentError>(())
            })();
            if let Err(error) = moved {
                self.rollback_committed();
                return Err(error);
            }
            self.committed.push(final_path);
        }
        if !self.stage_dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.stage_dir);
        }
        Ok(())
    }

    /// Remove files moved by `commit_files` when the following JSON write fails.
    pub fn rollback_committed(&mut self) {
        for path in self.committed.drain(..) {
            let _ = fs::remove_file(path);
        }
        let todo_ids: HashSet<String> = self
            .attachments
            .iter()
            .flat_map(|attachment| {
                std::iter::once(attachment.path.as_str())
                    .chain(attachment.thumbnail_path.as_deref())
            })
            .filter_map(managed_todo_id)
            .collect();
        for todo_id in todo_ids {
            prune_todo_dir(&todo_id);
        }
    }
}

impl Drop for StagedAttachmentBatch {
    fn drop(&mut self) {
        if !self.stage_dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.stage_dir);
        }
    }
}

/// Resolve and stage files without mutating `todos.json`.
pub fn stage_attachments(
    todo_id: &str,
    raw_paths: &[String],
    existing: &[TodoAttachment],
    cwd: &Path,
) -> Result<StagedAttachmentBatch, AttachmentError> {
    stage_attachments_at(
        &crate::paths::todo_attachments_dir(),
        &crate::paths::home(),
        todo_id,
        raw_paths,
        existing,
        cwd,
    )
}

fn stage_attachments_at(
    storage_root: &Path,
    home: &Path,
    todo_id: &str,
    raw_paths: &[String],
    existing: &[TodoAttachment],
    cwd: &Path,
) -> Result<StagedAttachmentBatch, AttachmentError> {
    validate_uuid(todo_id)?;
    if raw_paths.is_empty() {
        return Ok(StagedAttachmentBatch::empty());
    }

    let mut seen: HashSet<String> = existing.iter().map(|a| a.source_path.clone()).collect();
    let mut resolved = Vec::new();
    for raw in raw_paths {
        validate_raw_path(raw)?;
        let expanded = crate::cli::file_attachment::expand_tilde(raw, home);
        let absolute = if expanded.is_absolute() {
            expanded
        } else {
            cwd.join(expanded)
        };
        let canonical =
            fs::canonicalize(&absolute).map_err(|_| AttachmentError::NotAFile(raw.clone()))?;
        let canonical_text = canonical.to_string_lossy().into_owned();
        if !seen.insert(canonical_text.clone()) {
            continue;
        }
        let metadata =
            fs::metadata(&canonical).map_err(|_| AttachmentError::NotAFile(raw.clone()))?;
        if !metadata.is_file() || File::open(&canonical).is_err() {
            return Err(AttachmentError::NotAFile(raw.clone()));
        }
        resolved.push((canonical, canonical_text, metadata));
    }

    if existing.len() + resolved.len() > MAX_ATTACHMENTS_PER_TODO {
        return Err(AttachmentError::TooMany);
    }
    if resolved.is_empty() {
        return Ok(StagedAttachmentBatch::empty());
    }

    let stage_dir = storage_root.join(format!(".stage-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&stage_dir).map_err(|source| AttachmentError::Io {
        path: stage_dir.display().to_string(),
        source,
    })?;
    harden_dir(storage_root);
    harden_dir(&stage_dir);

    let mut attachments = Vec::with_capacity(resolved.len());
    let mut staged = Vec::new();
    for (source_path, source_text, metadata) in resolved {
        let id = uuid::Uuid::new_v4().to_string();
        let name = source_path
            .file_name()
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let safe_name = sanitize_filename(&name);
        let final_dir = storage_root.join(todo_id);
        let final_file = final_dir.join("files").join(format!("{id}-{safe_name}"));
        let staged_file = stage_dir.join(format!("{id}-file"));
        let storage = match bounded_copy(&source_path, &staged_file) {
            Ok(storage) => storage,
            Err(error) => {
                let _ = fs::remove_dir_all(&stage_dir);
                return Err(error);
            }
        };
        let effective_path = match storage {
            TodoAttachmentStorage::Managed => {
                staged.push((staged_file, final_file.clone()));
                final_file.to_string_lossy().into_owned()
            }
            TodoAttachmentStorage::Reference => {
                let _ = fs::remove_file(&staged_file);
                source_text.clone()
            }
        };

        let is_image = crate::cli::file_attachment::is_image_ext(&name);
        let final_thumb = final_dir.join("thumbnails").join(format!("{id}.png"));
        let staged_thumb = stage_dir.join(format!("{id}-thumb.png"));
        let thumbnail_path = if is_image && generate_thumbnail(&source_path, &staged_thumb).is_ok()
        {
            staged.push((staged_thumb, final_thumb.clone()));
            Some(final_thumb.to_string_lossy().into_owned())
        } else {
            let _ = fs::remove_file(&staged_thumb);
            None
        };

        attachments.push(TodoAttachment {
            id,
            name,
            size: metadata.len(),
            is_image,
            source_path: source_text,
            path: effective_path,
            storage,
            thumbnail_path,
            source_modified_ms: modified_ms(&metadata),
        });
    }

    Ok(StagedAttachmentBatch {
        attachments,
        staged,
        stage_dir,
        storage_root: storage_root.to_path_buf(),
        committed: Vec::new(),
    })
}

fn bounded_copy(
    source: &Path,
    destination: &Path,
) -> Result<TodoAttachmentStorage, AttachmentError> {
    let mut input = File::open(source).map_err(|source_err| AttachmentError::Io {
        path: source.display().to_string(),
        source: source_err,
    })?;
    let mut output = File::create(destination).map_err(|source_err| AttachmentError::Io {
        path: destination.display().to_string(),
        source: source_err,
    })?;
    let mut total = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|source_err| AttachmentError::Io {
                path: source.display().to_string(),
                source: source_err,
            })?;
        if read == 0 {
            output.flush().map_err(|source_err| AttachmentError::Io {
                path: destination.display().to_string(),
                source: source_err,
            })?;
            harden_file(destination);
            return Ok(TodoAttachmentStorage::Managed);
        }
        total += read as u64;
        if total > MANAGED_MAX_BYTES {
            drop(output);
            let _ = fs::remove_file(destination);
            return Ok(TodoAttachmentStorage::Reference);
        }
        output
            .write_all(&buffer[..read])
            .map_err(|source_err| AttachmentError::Io {
                path: destination.display().to_string(),
                source: source_err,
            })?;
    }
}

fn generate_thumbnail(source: &Path, destination: &Path) -> Result<(), String> {
    let reader = image::ImageReader::open(source)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;
    if u64::from(width) * u64::from(height) > MAX_THUMBNAIL_PIXELS {
        return Err("image dimensions exceed thumbnail safety limit".to_string());
    }
    let image = image::ImageReader::open(source)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    let thumb = if image.width() <= THUMBNAIL_MAX_EDGE && image.height() <= THUMBNAIL_MAX_EDGE {
        image
    } else {
        image.thumbnail(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE)
    };
    thumb
        .save_with_format(destination, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    harden_file(destination);
    Ok(())
}

pub fn cleanup_attachment(attachment: &TodoAttachment) {
    if attachment.storage == TodoAttachmentStorage::Managed {
        safe_remove_managed_file(&attachment.path);
    }
    if let Some(path) = &attachment.thumbnail_path {
        safe_remove_managed_file(path);
    }
}

pub fn cleanup_attachments(attachments: &[TodoAttachment]) {
    for attachment in attachments {
        cleanup_attachment(attachment);
    }
    let todo_ids: HashSet<String> = attachments
        .iter()
        .flat_map(|attachment| {
            std::iter::once(attachment.path.as_str()).chain(attachment.thumbnail_path.as_deref())
        })
        .filter_map(managed_todo_id)
        .collect();
    for id in todo_ids {
        prune_todo_dir(&id);
    }
}

fn safe_remove_managed_file(raw: &str) {
    let root = crate::paths::todo_attachments_dir();
    let Some(path) = validated_managed_file_at(&root, None, raw, None) else {
        return;
    };
    if let Err(error) = fs::remove_file(&path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "AskHuman: failed to remove managed todo attachment {}: {error}",
                path.display()
            );
        }
    }
}

fn managed_todo_id(raw: &str) -> Option<String> {
    let root = crate::paths::todo_attachments_dir();
    validated_managed_file_at(&root, None, raw, None).and_then(|path| {
        path.strip_prefix(root)
            .ok()?
            .components()
            .next()?
            .as_os_str()
            .to_str()
            .map(str::to_string)
    })
}

fn prune_todo_dir(todo_id: &str) {
    if validate_uuid(todo_id).is_err() {
        return;
    }
    let dir = crate::paths::todo_attachment_dir(todo_id);
    if fs::symlink_metadata(&dir).map_or(true, |metadata| {
        !metadata.file_type().is_dir() || metadata.file_type().is_symlink()
    }) {
        return;
    }
    for child in [dir.join("files"), dir.join("thumbnails")] {
        if fs::symlink_metadata(&child).map_or(true, |metadata| {
            !metadata.file_type().is_dir() || metadata.file_type().is_symlink()
        }) {
            continue;
        }
        if fs::read_dir(&child)
            .ok()
            .is_some_and(|mut entries| entries.next().is_none())
        {
            let _ = fs::remove_dir(&child);
        }
    }
    if fs::read_dir(&dir)
        .ok()
        .is_some_and(|mut entries| entries.next().is_none())
    {
        let _ = fs::remove_dir(dir);
    }
}

#[derive(Debug, Default, Clone)]
pub struct TodoDelivery {
    pub files: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn prepare_delivery(
    todo_id: &str,
    expected: &[TodoAttachmentSnapshot],
    current: Option<&[TodoAttachment]>,
    request_id: &str,
) -> TodoDelivery {
    if validate_uuid(todo_id).is_err() || validate_uuid(request_id).is_err() {
        return TodoDelivery {
            files: Vec::new(),
            warnings: vec!["Todo attachment delivery has an invalid identifier".to_string()],
        };
    }
    prepare_delivery_at(
        &crate::paths::todo_attachments_dir(),
        &crate::paths::todo_delivery_dir(request_id),
        todo_id,
        expected,
        current,
    )
}

fn prepare_delivery_at(
    storage_root: &Path,
    target_dir: &Path,
    todo_id: &str,
    expected: &[TodoAttachmentSnapshot],
    current: Option<&[TodoAttachment]>,
) -> TodoDelivery {
    let mut delivery = TodoDelivery::default();
    let current = current.unwrap_or(&[]);
    let current_ids: HashSet<&str> = current.iter().map(|a| a.id.as_str()).collect();

    for snapshot in expected {
        if !current_ids.contains(snapshot.id.as_str()) {
            delivery.warnings.push(format!(
                "Todo attachment unavailable or removed: {} ({})",
                snapshot.name, snapshot.source_path
            ));
            continue;
        }
        let Some(attachment) = current.iter().find(|a| a.id == snapshot.id) else {
            continue;
        };
        if !Path::new(&attachment.path).is_file() {
            delivery.warnings.push(format!(
                "Todo attachment unavailable: {} ({})",
                attachment.name, attachment.source_path
            ));
            continue;
        }
        if attachment.storage == TodoAttachmentStorage::Reference {
            delivery.files.push(attachment.path.clone());
            continue;
        }
        let Some(source) =
            validated_managed_file_at(storage_root, Some(todo_id), &attachment.path, Some("files"))
        else {
            delivery.warnings.push(format!(
                "Todo attachment has an unsafe managed path: {} ({})",
                attachment.name, attachment.source_path
            ));
            continue;
        };
        if fs::create_dir_all(target_dir).is_err() {
            delivery.warnings.push(format!(
                "Failed to prepare todo attachment: {} ({})",
                attachment.name, attachment.source_path
            ));
            continue;
        }
        harden_dir(target_dir);
        let target = target_dir.join(format!(
            "{}-{}",
            attachment.id,
            sanitize_filename(&attachment.name)
        ));
        let copied = fs::hard_link(&source, &target)
            .or_else(|_| fs::copy(&source, &target).map(|_| ()))
            .is_ok();
        if copied {
            harden_file(&target);
            delivery.files.push(target.to_string_lossy().into_owned());
        } else {
            let _ = fs::remove_file(&target);
            delivery.warnings.push(format!(
                "Failed to prepare todo attachment: {} ({})",
                attachment.name, attachment.source_path
            ));
        }
    }
    delivery
}

pub fn warning_block(warnings: &[String]) -> Option<String> {
    if warnings.is_empty() {
        return None;
    }
    Some(format!("Attachment status:\n- {}", warnings.join("\n- ")))
}

pub fn cleanup_delivery(request_id: &str) {
    if validate_uuid(request_id).is_err() {
        return;
    }
    let dir = crate::paths::todo_delivery_dir(request_id);
    let request_dir = crate::paths::request_temp_dir(request_id);
    let real_request_dir = fs::symlink_metadata(&request_dir)
        .is_ok_and(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink());
    let real_delivery_dir = fs::symlink_metadata(&dir)
        .is_ok_and(|metadata| metadata.file_type().is_dir() && !metadata.file_type().is_symlink());
    if real_request_dir
        && real_delivery_dir
        && dir.starts_with(std::env::temp_dir().join("askhuman"))
    {
        let _ = fs::remove_dir_all(dir);
    }
}

/// Best-effort crash recovery for old staged/unreferenced files. The age gate prevents an hourly
/// daemon sweep from racing an in-progress cross-process Todo transaction.
pub fn cleanup_orphans(referenced: &HashSet<PathBuf>) {
    let root = crate::paths::todo_attachments_dir();
    let Ok(children) = fs::read_dir(&root) else {
        return;
    };
    for child in children.flatten() {
        let path = child.path();
        let name = child.file_name().to_string_lossy().into_owned();
        if name.starts_with(".stage-") {
            if old_enough(&path) {
                match child.file_type() {
                    Ok(kind) if kind.is_dir() => {
                        let _ = fs::remove_dir_all(path);
                    }
                    Ok(kind) if kind.is_symlink() => {
                        let _ = fs::remove_file(path);
                    }
                    _ => {}
                }
            }
            continue;
        }
        if uuid::Uuid::parse_str(&name).is_err()
            || child.file_type().map_or(true, |kind| !kind.is_dir())
        {
            continue;
        }
        for subdir in [path.join("files"), path.join("thumbnails")] {
            if fs::symlink_metadata(&subdir).map_or(true, |metadata| {
                !metadata.file_type().is_dir() || metadata.file_type().is_symlink()
            }) {
                continue;
            }
            let Ok(files) = fs::read_dir(&subdir) else {
                continue;
            };
            for file in files.flatten() {
                let candidate = file.path();
                if !referenced.contains(&candidate) && old_enough(&candidate) {
                    let _ = fs::remove_file(candidate);
                }
            }
        }
        prune_todo_dir(&name);
    }
}

fn old_enough(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= ORPHAN_MAX_AGE)
}

pub fn read_thumbnail_data_url(todo_id: &str, attachment: &TodoAttachment) -> Option<String> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;

    let thumbnail = attachment.thumbnail_path.as_ref()?;
    let root = crate::paths::todo_attachments_dir();
    let thumbnail = validated_managed_file_at(&root, Some(todo_id), thumbnail, Some("thumbnails"))?;
    if attachment.storage == TodoAttachmentStorage::Reference {
        let source_meta = fs::metadata(&attachment.path).ok()?;
        let thumb_meta = fs::metadata(&thumbnail).ok();
        let stale = match (
            source_meta.modified().ok(),
            thumb_meta.and_then(|m| m.modified().ok()),
        ) {
            (Some(source), Some(thumb)) => source > thumb,
            (_, None) => true,
            _ => false,
        };
        if stale && generate_thumbnail(Path::new(&attachment.path), &thumbnail).is_err() {
            return None;
        }
    }
    let metadata = fs::symlink_metadata(&thumbnail).ok()?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let bytes = fs::read(&thumbnail).ok()?;
    Some(format!("data:image/png;base64,{}", B64.encode(bytes)))
}

/// Accept only `<root>/<todo-uuid>/{files|thumbnails}/<one filename>` and reject symlinked
/// parent directories. The stored JSON is local state, but cleanup and thumbnail reads must still
/// fail closed if that state is corrupted or manually edited.
fn validated_managed_file_at(
    root: &Path,
    expected_todo_id: Option<&str>,
    raw: &str,
    expected_area: Option<&str>,
) -> Option<PathBuf> {
    use std::path::Component;

    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return None;
    }
    let relative = path.strip_prefix(root).ok()?;
    let mut components = relative.components();
    let Component::Normal(todo_component) = components.next()? else {
        return None;
    };
    let todo_id = todo_component.to_str()?;
    uuid::Uuid::parse_str(todo_id).ok()?;
    if expected_todo_id.is_some_and(|expected| expected != todo_id) {
        return None;
    }
    let Component::Normal(area_component) = components.next()? else {
        return None;
    };
    let area = area_component.to_str()?;
    if !matches!(area, "files" | "thumbnails")
        || expected_area.is_some_and(|expected| expected != area)
    {
        return None;
    }
    let Component::Normal(_) = components.next()? else {
        return None;
    };
    if components.next().is_some() {
        return None;
    }

    for parent in [
        root.to_path_buf(),
        root.join(todo_id),
        root.join(todo_id).join(area),
    ] {
        let metadata = fs::symlink_metadata(parent).ok()?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return None;
        }
    }
    Some(path)
}

fn validate_uuid(value: &str) -> Result<(), AttachmentError> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| AttachmentError::InvalidTodoId)
}

fn validate_raw_path(value: &str) -> Result<(), AttachmentError> {
    if value.trim().is_empty() || value.contains(['\0', '\r', '\n']) {
        Err(AttachmentError::InvalidPath(value.to_string()))
    } else {
        Ok(())
    }
}

fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

fn modified_ms(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

#[cfg(unix)]
fn harden_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn harden_dir(_path: &Path) {}

#[cfg(unix)]
fn harden_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden_file(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_sanitization_keeps_a_safe_basename() {
        assert_eq!(sanitize_filename("../../a:b?.png"), "ab.png");
        assert_eq!(sanitize_filename("...."), "file");
    }

    #[test]
    fn warning_block_is_omitted_when_empty() {
        assert!(warning_block(&[]).is_none());
        assert!(warning_block(&["missing".into()])
            .unwrap()
            .contains("missing"));
    }

    #[test]
    fn stages_managed_reference_duplicate_and_thumbnail_files() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("storage");
        let cwd = temp.path().join("cwd");
        fs::create_dir_all(&cwd).unwrap();
        let small = cwd.join("small.txt");
        fs::write(&small, b"hello").unwrap();
        let large = cwd.join("large.bin");
        File::create(&large)
            .unwrap()
            .set_len(MANAGED_MAX_BYTES + 1)
            .unwrap();
        let image_path = cwd.join("photo.png");
        image::RgbaImage::from_pixel(512, 256, image::Rgba([10, 20, 30, 255]))
            .save(&image_path)
            .unwrap();
        let todo_id = uuid::Uuid::new_v4().to_string();
        let raw = vec![
            "small.txt".to_string(),
            "small.txt".to_string(),
            "large.bin".to_string(),
            "photo.png".to_string(),
        ];
        let mut batch =
            stage_attachments_at(&storage, temp.path(), &todo_id, &raw, &[], &cwd).unwrap();
        assert_eq!(batch.attachments.len(), 3);
        assert_eq!(batch.attachments[0].storage, TodoAttachmentStorage::Managed);
        assert_eq!(
            batch.attachments[1].storage,
            TodoAttachmentStorage::Reference
        );
        assert!(batch.attachments[2].thumbnail_path.is_some());
        batch.commit_files().unwrap();
        assert!(Path::new(&batch.attachments[0].path).is_file());
        assert_eq!(
            batch.attachments[1].path,
            fs::canonicalize(&large).unwrap().to_string_lossy()
        );
        let thumbnail =
            image::ImageReader::open(batch.attachments[2].thumbnail_path.as_deref().unwrap())
                .unwrap()
                .decode()
                .unwrap();
        assert_eq!((thumbnail.width(), thumbnail.height()), (128, 64));
    }

    #[test]
    fn delivery_copies_managed_files_and_warns_for_removed_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join("storage");
        let todo_id = uuid::Uuid::new_v4().to_string();
        let source = storage.join(&todo_id).join("files").join("managed.txt");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"payload").unwrap();
        let attachment = TodoAttachment {
            id: uuid::Uuid::new_v4().to_string(),
            name: "managed.txt".into(),
            size: 7,
            is_image: false,
            source_path: "/original/managed.txt".into(),
            path: source.to_string_lossy().into_owned(),
            storage: TodoAttachmentStorage::Managed,
            thumbnail_path: None,
            source_modified_ms: None,
        };
        let delivery_dir = temp.path().join("delivery");
        let delivered = prepare_delivery_at(
            &storage,
            &delivery_dir,
            &todo_id,
            &[attachment.snapshot()],
            Some(std::slice::from_ref(&attachment)),
        );
        assert!(delivered.warnings.is_empty());
        assert_eq!(delivered.files.len(), 1);
        assert_eq!(fs::read(&delivered.files[0]).unwrap(), b"payload");

        let removed = prepare_delivery_at(
            &storage,
            &delivery_dir,
            &todo_id,
            &[attachment.snapshot()],
            Some(&[]),
        );
        assert!(removed.files.is_empty());
        assert_eq!(removed.warnings.len(), 1);
    }

    #[test]
    fn bounded_copy_honors_exact_managed_threshold() {
        let temp = tempfile::tempdir().unwrap();
        for (size, expected) in [
            (0, TodoAttachmentStorage::Managed),
            (1, TodoAttachmentStorage::Managed),
            (MANAGED_MAX_BYTES, TodoAttachmentStorage::Managed),
            (MANAGED_MAX_BYTES + 1, TodoAttachmentStorage::Reference),
        ] {
            let source = temp.path().join(format!("source-{size}"));
            File::create(&source).unwrap().set_len(size).unwrap();
            let destination = temp.path().join(format!("destination-{size}"));
            assert_eq!(bounded_copy(&source, &destination).unwrap(), expected);
            assert_eq!(
                destination.exists(),
                expected == TodoAttachmentStorage::Managed
            );
        }
    }

    #[test]
    fn twenty_first_unique_attachment_rejects_the_entire_batch() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("cwd");
        fs::create_dir_all(&cwd).unwrap();
        let mut paths = Vec::new();
        for index in 0..=MAX_ATTACHMENTS_PER_TODO {
            let name = format!("file-{index}.txt");
            fs::write(cwd.join(&name), b"x").unwrap();
            paths.push(name);
        }
        let storage = temp.path().join("storage");
        let result = stage_attachments_at(
            &storage,
            temp.path(),
            &uuid::Uuid::new_v4().to_string(),
            &paths,
            &[],
            &cwd,
        );
        assert!(matches!(result, Err(AttachmentError::TooMany)));
        assert!(!storage.exists());
    }

    #[test]
    fn managed_path_validation_rejects_traversal_and_symlinked_parents() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("storage");
        let todo_id = uuid::Uuid::new_v4().to_string();
        let files = root.join(&todo_id).join("files");
        fs::create_dir_all(&files).unwrap();
        let valid = files.join("safe.txt");
        fs::write(&valid, b"safe").unwrap();
        assert_eq!(
            validated_managed_file_at(
                &root,
                Some(&todo_id),
                valid.to_str().unwrap(),
                Some("files")
            ),
            Some(valid)
        );

        let traversal = root
            .join(&todo_id)
            .join("files")
            .join("..")
            .join("..")
            .join("outside.txt");
        assert!(validated_managed_file_at(
            &root,
            Some(&todo_id),
            traversal.to_str().unwrap(),
            Some("files")
        )
        .is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let linked_todo = uuid::Uuid::new_v4().to_string();
            let outside = temp.path().join("outside");
            fs::create_dir_all(outside.join("files")).unwrap();
            symlink(&outside, root.join(&linked_todo)).unwrap();
            let linked = root.join(&linked_todo).join("files").join("unsafe.txt");
            assert!(validated_managed_file_at(
                &root,
                Some(&linked_todo),
                linked.to_str().unwrap(),
                Some("files")
            )
            .is_none());
        }
    }
}
