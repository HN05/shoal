//! Read-only daemon diagnostics, separate from workspace recovery mutations.
use std::{collections::HashSet, ffi::OsStr, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    daemon::workspace::Manager,
    git,
    state::states,
    tools::{Dependency, Tool},
};

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
    Tool::dependencies()
        .map(|(tool, dependency)| {
            let program = tool.program();
            let executable = path.and_then(|path| {
                std::env::split_paths(path)
                    .map(|dir| dir.join(program))
                    .find(|candidate| {
                        candidate.metadata().is_ok_and(|meta| {
                            meta.is_file() && meta.permissions().mode() & 0o111 != 0
                        })
                    })
            });
            let (status, message) = match executable {
                Some(path) => (CheckStatus::Ok, format!("{} on the daemon's PATH", path.display())),
                None => (
                    match dependency { Dependency::Required => CheckStatus::Error, Dependency::Optional(_) => CheckStatus::Warning },
                    format!(
                        "{program} is missing or not executable on the daemon's PATH{}; install it and restart the daemon with the updated PATH",
                        match dependency { Dependency::Required => String::new(), Dependency::Optional(reason) => format!(" ({reason})") }
                    ),
                ),
            };
            Check::new(tool.check_name(), status, message)
        })
        .collect()
}

pub fn worktrees_check_name(repository: &str) -> String {
    format!("worktrees:{repository}")
}

/// Repository names belong to the daemon and are unknown when it is unavailable.
pub fn unavailable_checks() -> Vec<Check> {
    Tool::dependencies()
        .map(|(tool, _)| tool.check_name())
        .chain([worktrees_check_name("*"), "workspaces".to_owned()])
        .map(|name| {
            Check::new(
                name,
                CheckStatus::Skipped,
                "Not checked: a reachable, matching daemon is required",
            )
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
            let name = worktrees_check_name(crate::forge::repository::name(&repo));
            // Take the same gate as creation/removal before reading ownership.
            let gate = self.git_gate(&repo.id).await;
            let _guard = gate.lock().await;
            let tracked: HashSet<PathBuf> = self
                .list_workspaces()
                .await?
                .into_iter()
                .map(|w| w.path)
                .collect();
            match git::worktrees(&repo.path, git::run).await {
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
    fn unavailable_dependencies_have_the_same_names_as_daemon_checks() {
        let checked: Vec<_> = dependencies(None)
            .into_iter()
            .map(|check| check.name)
            .collect();
        let skipped: Vec<_> = unavailable_checks()
            .into_iter()
            .filter(|check| check.name.starts_with("dependency:"))
            .map(|check| check.name)
            .collect();
        assert_eq!(skipped, checked);
    }

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
