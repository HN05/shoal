//! Holding a tracked agent's `git push` for approval, or refusing it, as its
//! `push` permission says. Fetches keep their URLs. See
//! `docs/design/permissions.md`.
use crate::paths::PathsExt;
use std::{os::unix::fs::PermissionsExt, path::Path};

use anyhow::{Context, Result};

use crate::{env, git, paths::Paths};

/// The remote helper the rewritten URLs reach.
const HELPER: &str = "git-remote-shoal";

/// The push URL rewrites an agent launch adds to the agent's Git settings,
/// and the directory with the remote helper they reach.
#[derive(Debug, Default)]
pub struct Launch {
    settings: Vec<(String, String)>,
    helper: Option<tempfile::TempDir>,
}

impl Launch {
    /// The rewrites for the remotes of the repository at `worktree`, and
    /// the helper they reach.
    pub async fn prepare(paths: &Paths, worktree: &Path) -> Result<Self> {
        let settings = git::push_gate::rewrites(worktree).await?;
        if settings.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self {
            settings,
            helper: Some(write_helper(paths)?),
        })
    }

    pub fn settings(&self) -> &[(String, String)] {
        &self.settings
    }

    /// Put the helper first on the child's PATH.
    pub fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(helper) = &self.helper {
            env::prepend_path(command, helper.path().as_os_str());
        }
    }
}

/// The helper: a launcher that hands Git's arguments to this binary.
fn write_helper(paths: &Paths) -> Result<tempfile::TempDir> {
    let directory = paths.push_gate_directory()?;
    std::env::join_paths([directory.path()]).with_context(|| {
        format!(
            "the push helper cannot go on PATH from {}",
            directory.path().display()
        )
    })?;
    let words = crate::workers::internal_command(paths, false, crate::workers::Worker::PushGate)?
        .into_iter()
        .map(|word| {
            word.into_string()
                .map_err(|word| anyhow::anyhow!("{} is not UTF-8", word.to_string_lossy()))
        })
        .collect::<Result<Vec<_>>>()?;
    let path = directory.path().join(HELPER);
    let invocation = crate::shell::quote(&words);
    std::fs::write(&path, format!("#!/bin/sh\nexec {invocation} \"$@\"\n"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(directory)
}
