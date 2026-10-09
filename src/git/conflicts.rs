//! Merge conflict checks for `shoal conflicts` and workspace watches.
use std::path::Path;

use anyhow::{Context, Result};

use crate::{
    daemon::workspace::Manager,
    git::{self, default_branch::DefaultBranchLookup},
    model::{ConflictCheck, Workspace},
    subprocess,
};

impl Manager {
    /// Whether the workspace's committed HEAD merges cleanly into `target`,
    /// which defaults to the base workspace's branch, then the default branch.
    pub async fn check_conflicts(
        &self,
        selector: &str,
        target: Option<String>,
    ) -> Result<ConflictCheck> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let (target, reference) = match target {
            Some(target) => (target.clone(), target),
            None => {
                let branch = self.conflict_target(&workspace).await?;
                let reference = git::local_ref(&branch);
                (branch, reference)
            }
        };
        let head = git::resolve_commit(&workspace.path, "HEAD", git::run_isolated).await?;
        let target_commit = git::resolve_commit(&workspace.path, &reference, git::run_isolated)
            .await
            .with_context(|| format!("cannot resolve {target}"))?;
        let files = merge_conflicts(&workspace.path, &head, &target_commit).await?;
        Ok(ConflictCheck {
            workspace_id: workspace.id,
            target,
            target_commit,
            head,
            conflicts: !files.is_empty(),
            files,
        })
    }

    async fn conflict_target(&self, workspace: &Workspace) -> Result<String> {
        if let Some(base) = &workspace.base_workspace {
            return Ok(base.branch.clone());
        }
        let repo = self.repository(workspace.repository_id.as_str()).await?;
        git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await
    }
}

/// The paths a merge of `head` and `target` leaves conflicted; empty when it
/// is clean. `git merge-tree` writes objects only, never the index, worktree
/// or refs.
async fn merge_conflicts(repo: &Path, head: &str, target: &str) -> Result<Vec<String>> {
    let mut command = git::isolated_command_without_submodules(repo);
    command.args([
        "merge-tree",
        "--write-tree",
        "--name-only",
        "--no-messages",
        "-z",
        head,
        target,
    ]);
    let output = subprocess::Run::new(command).capture().await;
    if output.as_ref().ok().and_then(|output| output.status.code()) != Some(1) {
        subprocess::checked_output("git merge-tree", output)
            .context("check merge conflicts; Git 2.38 or newer is required")?;
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8(output?.stdout).context("Git paths are not UTF-8")?;
    // The tree ID, then each conflicted path, all NUL-terminated.
    Ok(stdout
        .split('\0')
        .skip(1)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::test_support::{commit, git, manager, repository};

    #[tokio::test]
    async fn conflicts_compare_committed_head_with_the_default_or_named_branch() {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let repo_id = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap()
            .id;
        let workspace = manager
            .create_workspace(&repo_id, "topic".into(), None, None, None)
            .await
            .unwrap();
        let check = manager.check_conflicts(&workspace.id, None).await.unwrap();
        assert_eq!(check.target, "main");
        assert!(!check.conflicts && check.files.is_empty());

        fs::write(workspace.path.join("tracked"), "workspace\n").unwrap();
        commit(&workspace.path, "tracked");
        fs::write(repo.join("tracked"), "main\n").unwrap();
        commit(&repo, "tracked");
        fs::write(repo.join("other"), "main\n").unwrap();
        commit(&repo, "other");
        let check = manager.check_conflicts(&workspace.id, None).await.unwrap();
        assert!(check.conflicts);
        assert_eq!(check.files, ["tracked"]);
        assert_eq!(
            check.target_commit,
            git(&repo, &["rev-parse", "main"]).trim()
        );
        assert_eq!(
            check.head,
            git(&workspace.path, &["rev-parse", "HEAD"]).trim()
        );
        assert_eq!(
            git(&workspace.path, &["status", "--porcelain"]),
            "",
            "the check leaves the worktree and index untouched"
        );

        git(&repo, &["branch", "release", "main~2"]);
        let check = manager
            .check_conflicts(&workspace.id, Some("release".into()))
            .await
            .unwrap();
        assert_eq!(check.target, "release");
        assert!(!check.conflicts);
        let error = manager
            .check_conflicts(&workspace.id, Some("missing".into()))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("cannot resolve missing"));

        let base = manager
            .create_workspace(&repo_id, "base".into(), None, None, None)
            .await
            .unwrap();
        manager
            .set_base_workspace(&workspace.id, Some(base.name.clone()))
            .await
            .unwrap();
        let check = manager.check_conflicts(&workspace.id, None).await.unwrap();
        assert_eq!(check.target, base.branch);
        assert_eq!(
            check.target_commit,
            git(&base.path, &["rev-parse", "HEAD"]).trim()
        );
    }
}
