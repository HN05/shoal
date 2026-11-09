//! Local merges and unmerges of workspace branches into the default branch.
use super::{MergeWarning, Repository, Workspace};
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PulledBranch {
    pub branch: String,
    pub repository_id: RepositoryId,
    pub previous_commit: String,
    pub commit: String,
    pub updated: bool,
    /// Why a merge source was left as it is instead of refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

impl PulledBranch {
    pub fn unchanged(repo: &Repository, branch: &str, commit: String, skipped: String) -> Self {
        Self {
            branch: branch.to_owned(),
            repository_id: repo.id.clone(),
            previous_commit: commit.clone(),
            commit,
            updated: false,
            skipped: Some(skipped),
        }
    }
}

/// A workspace branch merged into its repository's default branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedBranch {
    pub workspace_id: SpaceId,
    pub repository_id: RepositoryId,
    pub branch: String,
    pub default_branch: String,
    pub previous_commit: String,
    pub commit: String,
    /// False when the default branch already contained the workspace branch.
    pub updated: bool,
    /// True when the default branch fast-forwarded instead of a merge commit.
    pub fast_forward: bool,
    /// How the default branch was refreshed from its upstream first.
    pub default_refresh: PulledBranch,
    /// The remote the default branch was pushed to after merging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_to: Option<String>,
}

/// A daemon-authorized merge, held under the repository Git gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePlan {
    pub workspace: Workspace,
    /// The workspace's branch, which merges.
    pub branch: String,
    pub repo: Repository,
    pub source: String,
    pub checkout: Option<PathBuf>,
    pub merge_temporaries: Vec<PathBuf>,
    pub default_refresh: PulledBranch,
    /// Where the default branch is pushed after the merge, if it is.
    pub push: Option<MergePush>,
}

/// A daemon-authorized unmerge, held under the repository Git gate: the
/// recorded merge of `branch` it undoes and the default branch it changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnmergePlan {
    pub workspace: Workspace,
    pub repo: Repository,
    pub default_branch: String,
    /// The default branch's checkout, if it has one.
    pub checkout: Option<PathBuf>,
    /// The merge's record.
    pub merge: i64,
    pub branch: String,
    /// The branch's commit that merged.
    pub source: String,
    /// The default branch's commit before the merge, and after it.
    pub previous: String,
    pub result: String,
    /// The default branch's commit when the unmerge was planned: the
    /// merge's result when it is the latest merge, a later commit otherwise.
    pub tip: String,
}

/// A merge undone: the default branch moved back from `previous_commit` to
/// `commit`, which no longer contains the branch's commits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unmerged {
    pub workspace_id: SpaceId,
    pub repository_id: RepositoryId,
    pub branch: String,
    pub default_branch: String,
    /// The branch's commit that had merged.
    pub source: String,
    pub previous_commit: String,
    pub commit: String,
    /// The commits after the merge replayed onto the commit before it; none
    /// when it was the latest merge.
    pub replayed: u64,
    /// The branch was gone and was created again at `source`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub branch_recreated: bool,
}

/// The upstream a merge pushes the default branch to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePush {
    pub remote: String,
    /// The ref on `remote` that receives the default branch.
    pub reference: String,
}

impl MergePush {
    /// Where `default` goes, as `origin`, or as `origin/release` when the
    /// remote branch has another name.
    pub fn destination(&self, default: &str) -> String {
        match self.branch() == default {
            true => self.remote.clone(),
            false => self.remote_branch(),
        }
    }

    /// The remote and its branch, as `origin/main`.
    pub fn remote_branch(&self) -> String {
        format!("{}/{}", self.remote, self.branch())
    }

    fn branch(&self) -> &str {
        self.reference
            .strip_prefix("refs/heads/")
            .unwrap_or(&self.reference)
    }
}

/// How a merge runs: whether it pushes, and the merge the user confirmed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeOptions {
    /// Push the default branch to its upstream afterwards.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub push: bool,
    /// What the user confirmed, which the merge must still be when it starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<MergeConfirmation>,
}

/// The commits, push destination and warning a user confirmed for a merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeConfirmation {
    pub head: String,
    /// The local default branch's commit the merge goes onto.
    pub default_commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<MergePush>,
    /// The sensitive files changed on both sides that the question named.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_on_both_sides: Vec<String>,
}

/// What merging a workspace branch into the default branch does, for the
/// user to confirm or approve before it happens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePreview {
    pub workspace_id: SpaceId,
    pub branch: String,
    /// The branch's commit that merges.
    pub head: String,
    pub default_branch: String,
    /// The local default branch's commit the merge goes onto.
    pub default_commit: String,
    /// Commits of the branch that the default branch lacks.
    pub commits: u64,
    /// Where the default branch is pushed afterwards, if it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_to: Option<MergePush>,
    /// Commits of the default branch that its push destination lacks, which
    /// the push publishes too.
    #[serde(default)]
    pub unpushed: u64,
    /// The sensitive files the merge brings to a default branch that changed
    /// them too, as the last check of the branch's head found them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<MergeWarning>,
    /// Open PRs the workspace watches, which `shoal pr merge` merges on the
    /// forge instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_prs: Vec<String>,
}

impl MergePreview {
    /// The merge as the user confirms it.
    pub fn confirmation(&self) -> MergeConfirmation {
        MergeConfirmation {
            head: self.head.clone(),
            default_commit: self.default_commit.clone(),
            push: self.push_to.clone(),
            changed_on_both_sides: self.changed_on_both_sides(),
        }
    }

    /// The default branch's commit when the push publishes commits of it
    /// besides the merge's.
    pub fn unpushed_default(&self) -> Option<String> {
        (self.push_to.is_some() && self.unpushed > 0).then(|| self.default_commit.clone())
    }

    /// The sensitive files the warning names, if there is one.
    pub fn changed_on_both_sides(&self) -> Vec<String> {
        self.warning
            .as_ref()
            .map(|warning| warning.files.clone())
            .unwrap_or_default()
    }

    /// The merge, its direction and what follows it, one sentence a line:
    /// `Merge fix-login (3 commits) into main, then push main to origin.`
    pub fn description(&self) -> String {
        self.sentences().join("\n")
    }

    /// [`Self::description`] on one line, cut to fit an approval reason.
    pub fn reason(&self) -> String {
        crate::validate::shorten(self.sentences().join(" "), crate::validate::MAX_REASON_LEN)
    }

    fn sentences(&self) -> Vec<String> {
        let (branch, default) = (&self.branch, &self.default_branch);
        let commits = count_commits(self.commits);
        let mut merge = format!("Merge {branch} ({commits}) into {default}");
        let mut unpushed = None;
        if let Some(push) = &self.push_to {
            let destination = push.destination(default);
            merge.push_str(&format!(", then push {default} to {destination}"));
            if self.unpushed > 0 {
                let (count, remote) = (count_commits(self.unpushed), push.remote_branch());
                unpushed = Some(format!(
                    "{default} has {count} that {remote} lacks, which the push publishes too."
                ));
            }
        }
        merge.push('.');
        let warning = self.warning.iter().map(|warning| format!("{warning}."));
        let prs = self
            .open_prs
            .iter()
            .map(|url| format!("PR {url} is open; shoal pr merge merges it on the forge instead."));
        std::iter::once(merge)
            .chain(unpushed)
            .chain(warning)
            .chain(prs)
            .collect()
    }
}

/// `1 commit` or `3 commits`.
fn count_commits(count: u64) -> String {
    match count {
        1 => "1 commit".to_owned(),
        count => format!("{count} commits"),
    }
}

#[cfg(test)]
mod tests;
