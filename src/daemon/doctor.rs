//! Daemon diagnostics, separate from workspace recovery mutations. They
//! change no work; asking forge hosts again records their answers.
use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};

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
            let executable = path.and_then(|path| crate::fsutil::find_executable(OsStr::new(program), path));
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

fn checkout_check_name(repository: &str) -> String {
    format!("checkout:{repository}")
}

/// Whether Git can use the working tree of a registered checkout.
async fn checkout(repo: &crate::model::Repository) -> Check {
    let name = checkout_check_name(crate::repository::name(repo));
    match crate::repository::bare_flag_problem(&repo.path).await {
        Ok(None) => Check::new(name, CheckStatus::Ok, "Git config does not set core.bare"),
        Ok(Some(problem)) => Check::new(
            name,
            CheckStatus::Warning,
            format!("{problem}, then check git status: the index may lag behind HEAD"),
        ),
        Err(error) => Check::new(
            name,
            CheckStatus::Error,
            format!("Could not check {}: {error:#}", repo.path.display()),
        ),
    }
}

const ROOT_DIR_CHECK: &str = "root_dir";

/// Whether the daemon can create files in `root_dir`, where new repositories
/// and their workspaces go, resolved as registration resolves it.
async fn root_dir(root: Result<PathBuf>) -> Check {
    let root =
        root.and_then(|root| crate::daemon::workspace::paths::canonical_with_missing_tail(&root));
    let checked = match root {
        Ok(root) => crate::daemon::access::ensure_writable(&root)
            .await
            .map(|()| root),
        Err(error) => Err(error),
    };
    match checked {
        Ok(root) => Check::new(
            ROOT_DIR_CHECK,
            CheckStatus::Ok,
            format!("The daemon can write to {}", root.display()),
        ),
        Err(error) => Check::new(
            ROOT_DIR_CHECK,
            CheckStatus::Error,
            format!("Cannot use root_dir: {error:#}"),
        ),
    }
}

/// Repository names belong to the daemon and are unknown when it is unavailable.
pub fn unavailable_checks() -> Vec<Check> {
    Tool::dependencies()
        .map(|(tool, _)| tool.check_name())
        .chain([
            "cleanup".to_owned(),
            ROOT_DIR_CHECK.to_owned(),
            checkout_check_name("*"),
            crate::daemon::forge_hosts::forge_check_name("*"),
            worktrees_check_name("*"),
            "workspaces".to_owned(),
        ])
        .map(|name| {
            Check::new(
                name,
                CheckStatus::Skipped,
                "Not checked: a reachable, matching daemon is required",
            )
        })
        .collect()
}

/// What Shoal owns under a repository's workspace directory.
struct OwnedWorktrees {
    tracked: HashSet<PathBuf>,
    seed: crate::seed::Layout,
    verified_seed: bool,
}

impl OwnedWorktrees {
    /// Warnings for the seed path when Shoal did not create it and for each
    /// Git worktree under `root`, other than the checkout, that Shoal does not own.
    fn problems(&self, checkout: &Path, root: &Path, trees: &[git::Worktree]) -> Vec<String> {
        let seed_path = self.seed.worktree();
        let foreign_seed = (seed_path.symlink_metadata().is_ok() && !self.verified_seed).then(|| {
            format!(
                "Seed path is not Shoal's seed worktree; Shoal will neither rebuild nor remove it: {}",
                seed_path.display()
            )
        });
        let untracked = trees
            .iter()
            .filter(|tree| {
                tree.path.starts_with(root)
                    && tree.path != checkout
                    && !(self.verified_seed && self.seed.is_worktree(&tree.path))
                    && !self.tracked.contains(&tree.path)
            })
            .map(|tree| {
                format!(
                    "Git worktree is not tracked by Shoal: {}",
                    tree.path.display()
                )
            });
        foreign_seed.into_iter().chain(untracked).collect()
    }
}

/// One warning per problem, or one passing check when there are none.
fn worktree_report(name: String, root: &Path, problems: Vec<String>) -> Vec<Check> {
    if problems.is_empty() {
        return vec![Check::new(
            name,
            CheckStatus::Ok,
            format!("No untracked Git worktrees under {}", root.display()),
        )];
    }
    problems
        .into_iter()
        .map(|problem| Check::new(&name, CheckStatus::Warning, problem))
        .collect()
}

impl Manager {
    pub async fn diagnose(&self) -> Result<Vec<Check>> {
        let mut checks = dependencies(std::env::var_os("PATH").as_deref());
        checks.push(self.cleanup_check());
        checks.push(root_dir(self.config().root_dir(&self.paths)).await);
        checks.extend(self.forge_host_checks().await?);
        for repo in self.repositories().await? {
            checks.push(checkout(&repo).await);
            checks.extend(self.worktree_checks(&repo).await?);
        }
        Ok(checks)
    }

    /// Git worktrees under the repository's workspace directory that Shoal
    /// does not own; none for a repository without one.
    async fn worktree_checks(&self, repo: &crate::model::Repository) -> Result<Vec<Check>> {
        let Some(root) = &repo.workspaces_dir else {
            return Ok(Vec::new());
        };
        let name = worktrees_check_name(crate::repository::name(repo));
        // Take the same gate as creation/removal before reading ownership.
        let _guard = self.lock_repository_git(&repo.id).await;
        let owned = self.owned_worktrees(repo, root).await?;
        Ok(match git::worktrees(&repo.path, git::run).await {
            Ok(trees) => worktree_report(name, root, owned.problems(&repo.path, root, &trees)),
            Err(error) => vec![Check::new(
                name,
                CheckStatus::Error,
                format!(
                    "Could not check worktrees under {}: {error:#}",
                    root.display()
                ),
            )],
        })
    }

    /// The caller holds the repository's Git gate so ownership stays current.
    async fn owned_worktrees(
        &self,
        repo: &crate::model::Repository,
        root: &Path,
    ) -> Result<OwnedWorktrees> {
        let tracked = self
            .list_workspaces()
            .await?
            .into_iter()
            .map(|w| w.path)
            .collect();
        let seed = crate::seed::Layout::new(root, &repo.id);
        let verified_seed = crate::daemon::seed::is_seed_worktree(repo, &seed)
            .await
            .unwrap_or(false);
        Ok(OwnedWorktrees {
            tracked,
            seed,
            verified_seed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

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

    #[tokio::test]
    async fn root_dir_reports_whether_the_daemon_can_write_there() {
        let root = tempfile::tempdir().unwrap();
        let check = root_dir(Ok(root.path().join("shoal"))).await;
        assert_eq!(check.status, CheckStatus::Ok);
        let file = root.path().join("file");
        fs::write(&file, "").unwrap();
        let check = root_dir(Ok(root.path().join("missing/../file"))).await;
        assert_eq!(check.status, CheckStatus::Error);
        let check = root_dir(Ok(file.clone())).await;
        assert_eq!(check.status, CheckStatus::Error);
        assert!(
            check.message.starts_with(&format!(
                "Cannot use root_dir: the Shoal daemon cannot write to {}: ",
                fs::canonicalize(&file).unwrap().display()
            )),
            "{}",
            check.message
        );
    }

    #[test]
    fn worktree_problems_name_untracked_worktrees_and_a_foreign_seed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let tree = |path: PathBuf| git::Worktree {
            path,
            branch: None,
            prunable: false,
            locked: false,
        };
        let seed = crate::seed::Layout::new(&root, "repo-id");
        let owned = OwnedWorktrees {
            tracked: HashSet::from([root.join("tracked")]),
            seed,
            verified_seed: false,
        };
        let trees = [
            tree(root.join(".checkout")),
            tree(root.join("tracked")),
            tree(root.join("stray")),
            tree(dir.path().join("elsewhere")),
        ];
        let checkout = root.join(".checkout");
        assert_eq!(
            owned.problems(&checkout, &root, &trees),
            [format!(
                "Git worktree is not tracked by Shoal: {}",
                root.join("stray").display()
            )]
        );
        let checks = worktree_report("worktrees:repo".to_owned(), &root, Vec::new());
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, CheckStatus::Ok);

        let seed_path = owned.seed.worktree();
        fs::create_dir_all(&seed_path).unwrap();
        let trees = [tree(seed_path.clone())];
        let problems = owned.problems(&checkout, &root, &trees);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("Seed path is not Shoal's seed worktree"));
        let verified = OwnedWorktrees {
            verified_seed: true,
            ..owned
        };
        assert!(verified.problems(&checkout, &root, &trees).is_empty());
        let checks = worktree_report("worktrees:repo".to_owned(), &root, problems);
        assert!(
            checks
                .iter()
                .all(|check| check.status == CheckStatus::Warning)
        );
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
        assert_eq!(checks[3].status, CheckStatus::Error);
        assert_eq!(checks[4].status, CheckStatus::Warning);
        assert!(
            dependencies(None)
                .iter()
                .all(|c| c.status != CheckStatus::Ok)
        );
    }
}
