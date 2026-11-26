//! Work of a workspace's own, which a refresh would lose, and the refusal
//! that names it and what deals with it.
use anyhow::Result;

use crate::{git, model::Workspace};

#[derive(Debug)]
pub(super) struct HoldsOwnWork(String);

impl std::fmt::Display for HoldsOwnWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HoldsOwnWork {}

const LISTED: usize = 5;

pub(super) struct OwnWork<'a> {
    /// Its commits after this one are its own.
    since: &'a str,
    /// Such as `that main lacks`.
    commits: String,
    /// Such as `git rebase main`.
    remedy: Option<String>,
    /// For a copy, the command that makes a fresh one. The files a copy
    /// started with are not its own work.
    fresh_copy: Option<&'a str>,
}

impl<'a> OwnWork<'a> {
    /// A rebase moves the commits onto `target`, which `label` names.
    pub(super) fn branch(target: &'a str, label: &str) -> Self {
        Self {
            since: target,
            commits: format!("that {label} lacks"),
            remedy: Some(format!("git rebase {label}")),
            fresh_copy: None,
        }
    }

    /// Its commits after `start`, where it started.
    pub(super) fn at_commit(start: &'a str) -> Self {
        Self {
            since: start,
            commits: "after its starting commit".to_owned(),
            remedy: None,
            fresh_copy: None,
        }
    }

    /// Its commits after `head`, the HEAD it was copied at, and files that
    /// differ from those it was copied with. `fresh` makes a fresh copy.
    pub(super) fn copy(head: &'a str, fresh: &'a str) -> Self {
        Self {
            since: head,
            commits: "since it was copied".to_owned(),
            remedy: None,
            fresh_copy: Some(fresh),
        }
    }

    /// Says what the commits or uncommitted changes are and what deals with
    /// them.
    pub(super) async fn refuse(&self, workspace: &Workspace) -> Result<()> {
        let mut held = Vec::new();
        if let Some(commits) = self.held_commits(workspace).await? {
            held.push(commits);
        }
        if !self.only_starting_files(workspace).await?
            && let Some(changes) = held_changes(workspace).await?
        {
            held.push(changes);
        }
        if held.is_empty() {
            return Ok(());
        }
        let fresh = (self.fresh_copy)
            .map(|command| format!("; or make a fresh copy with {command}"))
            .unwrap_or_default();
        Err(HoldsOwnWork(format!(
            "workspace {} holds work of its own, which refresh never carries over: {}{fresh}",
            workspace.name,
            held.join("; and ")
        ))
        .into())
    }

    /// Compares the files as cleanup does.
    async fn only_starting_files(&self, workspace: &Workspace) -> Result<bool> {
        Ok(self.fresh_copy.is_some() && crate::removal::holds_starting_files(workspace).await?)
    }

    /// None without any.
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

/// None in a clean worktree.
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

/// The first of `items` joined, with how many more of `total` there are.
fn listed(items: &[impl AsRef<str>], total: usize) -> String {
    let shown: Vec<&str> = items.iter().take(LISTED).map(AsRef::as_ref).collect();
    match total.saturating_sub(shown.len()) {
        0 => shown.join(", "),
        more => format!("{}, and {more} more", shown.join(", ")),
    }
}
