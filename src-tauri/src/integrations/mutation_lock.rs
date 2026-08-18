//! Cross-process serialization for integration artifact edits.

use anyhow::{Context, Result};

pub struct IntegrationMutationLock(crate::file_lock::FileLock);

impl IntegrationMutationLock {
    pub fn acquire() -> Result<Self> {
        let path = crate::paths::integrations_lock_file();
        crate::file_lock::FileLock::exclusive(&path)
            .map(Self)
            .with_context(|| format!("failed to lock {}", path.display()))
    }
}
