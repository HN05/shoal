//! Restart a managed daemon after its installed executable is replaced.
use crate::fsutil::FileIdentity;
use std::path::PathBuf;

pub(super) struct Update {
    pub(super) path: PathBuf,
    original: FileIdentity,
}

impl Update {
    /// Record the executable this daemon was started from. The daemon calls
    /// this before any startup work, while the path still names the running
    /// binary, so a replacement during startup is already pending.
    pub(super) fn new(managed: bool) -> Option<Self> {
        if !managed {
            return None;
        }
        let path = crate::service::executable(None).ok()?;
        let original = FileIdentity::of(&path).ok()?;
        Some(Self { path, original })
    }

    pub(super) fn pending(&self) -> bool {
        crate::fsutil::executables::is_executable(&self.path).unwrap_or(false)
            && self.original.replaced_at(&self.path)
    }
}
