//! Read-only daemon diagnostics, separate from workspace recovery mutations.
use std::{collections::HashSet, ffi::OsStr, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{git, state::states, workspace::Manager};

states!(CheckStatus {
    Ok => "ok",
    Warning => "warning",
    Error => "error",
    Skipped => "skipped",
});

#[derive(Debug, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

impl Check {
    pub fn new(name: impl Into<String>, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status,
            message: message.into(),
        }
    }
}

fn dependencies(path: Option<&OsStr>) -> Vec<Check> {
    ["git", "wt", "lsof", "fzf"]
        .into_iter()
        .map(|tool| {
            let executable = path.and_then(|path| {
                std::env::split_paths(path)
                    .map(|dir| dir.join(tool))
                    .find(|candidate| {
                        candidate.metadata().is_ok_and(|meta| {
                            meta.is_file() && meta.permissions().mode() & 0o111 != 0
                        })
                    })
            });
            let (status, message) = match executable {
                Some(path) => (CheckStatus::Ok, format!("{} on the daemon's PATH", path.display())),
                None => (
                    if tool == "fzf" { CheckStatus::Warning } else { CheckStatus::Error },
                    format!(
                        "{tool} is missing or not executable on the daemon's PATH{}; install it and restart the daemon with the updated PATH",
                        if tool == "fzf" { " (needed for interactive pickers)" } else { "" }
                    ),
                ),
            };
            Check::new(format!("dependency:{tool}"), status, message)
        })
        .collect()
}

impl Manager {
    pub async fn diagnose(&self) -> Result<Vec<Check>> {
        let mut checks = dependencies(std::env::var_os("PATH").as_deref());
        for repo in self.repositories().await? {
            let Some(root) = &repo.workspaces_dir else {
                continue;
            };
            let name = format!("worktrees:{}", crate::forge::repository::name(&repo));
            // Take the same gate as creation/removal before reading ownership.
            let gate = self.git_gate(&repo.id).await;
            let _guard = gate.lock().await;
            let tracked: HashSet<PathBuf> = self
                .list_workspaces()
                .await?
                .into_iter()
                .map(|w| w.path)
                .collect();
            match git::worktrees(&repo.path).await {
                Ok(trees) => {
                    let mut untracked = false;
                    for tree in trees {
                        if tree.path.starts_with(root)
                            && tree.path != repo.path
                            && !tracked.contains(&tree.path)
                        {
                            untracked = true;
                            checks.push(Check::new(
                                &name,
                                CheckStatus::Warning,
                                format!(
                                    "Git worktree is not tracked by Shoal: {}",
                                    tree.path.display()
                                ),
                            ));
                        }
                    }
                    if !untracked {
                        checks.push(Check::new(
                            name,
                            CheckStatus::Ok,
                            format!("No untracked Git worktrees under {}", root.display()),
                        ));
                    }
                }
                Err(error) => checks.push(Check::new(
                    name,
                    CheckStatus::Error,
                    format!(
                        "Could not check worktrees under {}: {error:#}",
                        root.display()
                    ),
                )),
            }
        }
        Ok(checks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dependencies_require_executable_files_on_the_supplied_path() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("git"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(root.path().join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.path().join("wt"), "not executable").unwrap();
        fs::create_dir(root.path().join("lsof")).unwrap();
        let checks = dependencies(Some(root.path().as_os_str()));
        assert_eq!(checks[0].status, CheckStatus::Ok);
        assert_eq!(checks[1].status, CheckStatus::Error);
        assert_eq!(checks[2].status, CheckStatus::Error);
        assert_eq!(checks[3].status, CheckStatus::Warning);
        assert!(
            dependencies(None)
                .iter()
                .all(|c| c.status != CheckStatus::Ok)
        );
    }
}
