//! Work of a workspace's own, which a refresh would lose, and the refusal
//! that names it and what deals with it.
use anyhow::{Result, ensure};

use crate::{git, model::Workspace};

/// The most commit subjects and changed paths a refusal lists.
const LISTED: usize = 5;

/// Where a workspace's own work starts, and what deals with its commits.
pub(super) struct OwnWork<'a> {
    /// The commit after which its commits are its own.
    since: &'a str,
    /// What those commits are, such as `that main lacks`.
    commits: String,
    /// The command that moves its commits, such as `git rebase main`.
    remedy: Option<String>,
}

impl<'a> OwnWork<'a> {
    /// A branch workspace's: its commits that `target`, which `label` names,
    /// lacks, which a rebase moves onto it.
    pub(super) fn branch(target: &'a str, label: &str) -> Self {
        Self {
            since: target,
            commits: format!("that {label} lacks"),
            remedy: Some(format!("git rebase {label}")),
        }
    }

    /// A workspace at a commit's: its commits after `start`, where it
    /// started.
    pub(super) fn at_commit(start: &'a str) -> Self {
        Self {
            since: start,
            commits: "after its starting commit".to_owned(),
            remedy: None,
        }
    }

    /// Refuse `workspace` when it holds commits of its own or uncommitted
    /// changes, saying what they are and what deals with them.
    pub(super) async fn refuse(&self, workspace: &Workspace) -> Result<()> {
        let mut held = Vec::new();
        if let Some(commits) = self.held_commits(workspace).await? {
            held.push(commits);
        }
        if let Some(changes) = held_changes(workspace).await? {
            held.push(changes);
        }
        ensure!(
            held.is_empty(),
            "workspace {} holds work of its own, which refresh never carries over: {}",
            workspace.name,
            held.join("; and ")
        );
        Ok(())
    }

    /// The commits of `workspace` after [`Self::since`], with what deals
    /// with them; none without any.
    async fn held_commits(&self, workspace: &Workspace) -> Result<Option<String>> {
        let path = &workspace.path;
        let count = git::status::commits_since(path, self.since).await?;
        if count == 0 {
            return Ok(None);
        }
        let subjects = git::status::subjects_since(path, self.since, LISTED).await?;
        let mut held = format!(
            "{count} commit(s) {} ({})",
            self.commits,
            listed(&subjects, usize::try_from(count)?)
        );
        if let Some(remedy) = &self.remedy {
            held += &format!("; move them with {remedy}");
        }
        Ok(Some(held))
    }
}

/// The uncommitted changes of `workspace`, with what deals with them; none
/// in a clean worktree.
async fn held_changes(workspace: &Workspace) -> Result<Option<String>> {
    let status = git::status::porcelain(&workspace.path).await?;
    let changed: Vec<&str> = (status.lines())
        .map(|line| line.get(3..).unwrap_or(line))
        .collect();
    Ok((!changed.is_empty()).then(|| {
        format!(
            "uncommitted changes to {}; commit, stash or discard them",
            listed(&changed, changed.len())
        )
    }))
}

/// The first of `items`, of `total` in all, joined, with how many more
/// there are.
fn listed(items: &[impl AsRef<str>], total: usize) -> String {
    let shown: Vec<&str> = items.iter().take(LISTED).map(AsRef::as_ref).collect();
    match total.saturating_sub(shown.len()) {
        0 => shown.join(", "),
        more => format!("{}, and {more} more", shown.join(", ")),
    }
}
