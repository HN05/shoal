//! Worktree Git configuration, independent of named profile selection.
use std::path::Path;

use anyhow::{Result, ensure};

/// Enable worktree-local config only when the shared layout supports it, then
/// apply settings without affecting other worktrees.
pub(crate) async fn apply_worktree_settings(
    worktree: &Path,
    settings: &[(String, String)],
) -> Result<()> {
    let shared = |key: &'static str, kind: &'static str| async move {
        super::run_isolated(
            worktree,
            &[
                "config",
                "--local",
                &format!("--type={kind}"),
                "--default",
                if kind == "bool" { "false" } else { "" },
                "--get",
                key,
            ],
        )
        .await
        .map(|value| value.trim().to_owned())
    };
    if shared("extensions.worktreeConfig", "bool").await? != "true" {
        ensure!(
            shared("core.worktree", "path").await?.is_empty()
                && shared("core.bare", "bool").await? != "true",
            "the repository's shared Git config sets core.worktree or core.bare; move them to the main worktree's config.worktree before using git profiles"
        );
        super::run_isolated(
            worktree,
            &["config", "--local", "extensions.worktreeConfig", "true"],
        )
        .await?;
    }
    for (key, value) in settings {
        super::run_isolated(worktree, &["config", "--worktree", key, value]).await?;
    }
    Ok(())
}

/// Whether Git takes paths in `worktree` to be the same whatever their
/// case, as on a case-insensitive filesystem, where it sets
/// `core.ignorecase`.
pub(crate) async fn ignores_case(worktree: &Path) -> Result<bool> {
    let value = super::run_isolated(
        worktree,
        &[
            "config",
            "--type=bool",
            "--default",
            "false",
            "--get",
            "core.ignorecase",
        ],
    )
    .await?;
    Ok(value.trim() == "true")
}
