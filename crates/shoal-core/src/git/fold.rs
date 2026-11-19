//! Folding a worktree's work into another worktree of its repository: its
//! branch merged into the other's by a merge method, and its unsaved files
//! carried over as unsaved changes there, never as a commit. Everything is
//! worked out before anything changes, so a fold that would not apply
//! cleanly, or would touch a file the receiving worktree has unsaved
//! changes to, changes nothing.
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use shoal_refs::CommitId;

use crate::forge::items::action::MergeMethod;

use super::{
    checkpoint,
    merge_into::{self, Source, Target},
    merge_methods::Applied,
    merge_tree::{self, Merge},
    run_isolated,
};

/// What a fold takes and where it goes.
pub struct Fold<'a> {
    pub repo: &'a Path,
    /// The branch that merges; none when no code merges.
    pub source: Option<Source<'a>>,
    /// The worktree whose unsaved files carry over; none to leave them.
    pub carry_from: Option<&'a Path>,
    /// The branch and worktree that take the work, at its HEAD.
    pub into: Target<'a>,
}

/// What a fold did.
#[derive(Debug, Default)]
pub struct Folded {
    pub merged: Option<merge_into::Merged>,
    /// The merged commits, each as `<short hash> <subject>`, oldest first.
    pub commits: Vec<String>,
    /// The files carried over as unsaved changes.
    pub carried: Vec<String>,
}

/// A worktree's unsaved files: its HEAD, a snapshot commit on top of it
/// with its files, and the paths they change.
struct Unsaved {
    head: CommitId,
    snapshot: CommitId,
    paths: Vec<String>,
    /// Its worktree takes paths to be the same whatever their case.
    ignores_case: bool,
}

/// What a fold changes, worked out before anything changes: the merge
/// commit and the commit `into` moves to, and the tree and paths of the
/// carried files.
struct Planned {
    merge: Option<(Applied, CommitId)>,
    carried: Option<(String, Vec<String>)>,
}

/// Fold as `fold` says.
pub async fn fold(fold: Fold<'_>) -> Result<Folded> {
    let planned = plan(&fold).await?;
    let mut folded = Folded::default();
    if let (Some(source), Some((applied, onto))) = (&fold.source, planned.merge) {
        folded.commits = commits(fold.repo, fold.into.commit, source.commit).await?;
        folded.merged = Some(apply(fold.repo, source, &fold.into, applied, onto).await?);
    }
    if let (Some(worktree), Some((tree, paths))) = (fold.into.worktree, planned.carried) {
        write_files(worktree, &tree, &paths).await?;
        folded.carried = paths;
    }
    Ok(folded)
}

/// Refuse now what folding as `fold` says would refuse, changing nothing.
pub async fn check(fold: Fold<'_>) -> Result<()> {
    plan(&fold).await.map(drop)
}

/// What folding as `fold` says changes, refused when the merge or the
/// carried files would not apply cleanly or would touch a file the
/// receiving worktree has unsaved changes to.
async fn plan(fold: &Fold<'_>) -> Result<Planned> {
    let worktree = (fold.into.worktree).context("the work folds into a worktree")?;
    let own = unsaved(worktree).await?;
    let merge = match &fold.source {
        Some(source) => Some(merge_onto(fold.repo, source, &fold.into, &own).await?),
        None => None,
    };
    let onto = merge.as_ref().map_or(fold.into.commit, |(_, onto)| onto);
    let carried = match fold.carry_from {
        Some(from) => carried_tree(fold.repo, from, (worktree, &own), onto).await?,
        None => None,
    };
    Ok(Planned { merge, carried })
}

/// Move `into`'s branch to `onto`, the commit merging `source` as `applied`
/// made.
async fn apply(
    repo: &Path,
    source: &Source<'_>,
    into: &Target<'_>,
    applied: Applied,
    onto: CommitId,
) -> Result<merge_into::Merged> {
    let updated = merge_into::advance(repo, source, into, applied).await?;
    Ok(merge_into::Merged {
        fast_forward: onto == *source.commit && source.method == MergeMethod::Merge,
        commit: onto,
        updated,
    })
}

/// The commit merging `source` into `into` makes, and the commit `into`'s
/// branch moves to, refused when it does not apply cleanly or changes a
/// file `own` has unsaved changes to.
async fn merge_onto(
    repo: &Path,
    source: &Source<'_>,
    into: &Target<'_>,
    own: &Unsaved,
) -> Result<(Applied, CommitId)> {
    let contained = super::is_ancestor(
        repo,
        source.commit.as_str(),
        into.commit.as_str(),
        super::isolated_command,
    );
    if contained.await? {
        return Ok((Applied::Unchanged, into.commit.clone()));
    }
    let applied = merge_into::merged_commit(repo, source, into).await?;
    let onto = match &applied {
        Applied::Commit(commit) => commit.clone(),
        Applied::Unchanged => into.commit.clone(),
        Applied::Conflicts(report) => {
            let reason = format!("{} does not apply cleanly", source.branch);
            bail!(
                "{reason} onto {}, so nothing changed\n{report}",
                into.branch
            );
        }
    };
    let changed = changed_paths(repo, into.commit.as_str(), onto.as_str()).await?;
    let touched = overlap(&changed, own);
    if !touched.is_empty() {
        bail!(
            "merging {} changes files {} has uncommitted changes to, so nothing changed: {}",
            source.branch,
            into.branch,
            touched.join(", ")
        );
    }
    Ok((applied, onto))
}

/// The tree with the unsaved files of the worktree `from` applied to
/// `onto`, and the paths where it differs from both `onto` and the
/// receiver's `own` files, which a rename in `onto` may have moved; none
/// when there are none. Refused when they change
/// a file the receiving worktree has `own` unsaved changes to or ignores,
/// or do not apply to `onto`.
async fn carried_tree(
    repo: &Path,
    from: &Path,
    (receiver, own): (&Path, &Unsaved),
    onto: &CommitId,
) -> Result<Option<(String, Vec<String>)>> {
    let theirs = unsaved(from).await?;
    if theirs.paths.is_empty() {
        return Ok(None);
    }
    let base = format!("--merge-base={}", theirs.head);
    let args = [base.as_str(), onto.as_str(), theirs.snapshot.as_str()];
    let tree = match merge_tree::write_tree(repo, &args).await? {
        Merge::Clean(stdout) => stdout.lines().next().unwrap_or_default().to_owned(),
        Merge::Conflicted(_) => {
            bail!("the uncommitted files do not apply onto the merged work, so nothing changed")
        }
    };
    // Only those the merged work and the receiver's own files lack, so a
    // fold that already carried them carries them no more.
    let carried = changed_paths(repo, onto.as_str(), &tree).await?;
    let lacking = changed_paths(repo, own.snapshot.as_str(), &tree).await?;
    let paths: Vec<String> = (carried.into_iter())
        .filter(|path| lacking.contains(path))
        .collect();
    if paths.is_empty() {
        return Ok(None);
    }
    ensure_carriable(&paths, receiver, own).await?;
    Ok(Some((tree, paths)))
}

/// Refuse carrying files to `paths` of the worktree `receiver` where they
/// would change a file it has `own` unsaved changes to, or replace a file
/// it ignores, which no checkpoint keeps.
async fn ensure_carriable(paths: &[String], receiver: &Path, own: &Unsaved) -> Result<()> {
    let touched = overlap(paths, own);
    if !touched.is_empty() {
        bail!(
            "the uncommitted files would change files with uncommitted changes there, so nothing changed: {}",
            touched.join(", ")
        );
    }
    let ignored = checkpoint::ignored_among(receiver, paths.iter().cloned()).await?;
    if !ignored.is_empty() {
        bail!(
            "the uncommitted files would replace ignored files there, so nothing changed: {}",
            ignored.join(", ")
        );
    }
    Ok(())
}

/// The unsaved files of `worktree`, which stay as they are.
async fn unsaved(worktree: &Path) -> Result<Unsaved> {
    let snapshot = checkpoint::snapshot(worktree, "Unsaved files", true).await?;
    let paths = changed_paths(worktree, snapshot.head.as_str(), snapshot.commit.as_str()).await?;
    Ok(Unsaved {
        head: snapshot.head,
        snapshot: snapshot.commit,
        paths,
        ignores_case: super::config::ignores_case(worktree).await?,
    })
}

/// The paths whose files differ between the trees of `from` and `to`.
async fn changed_paths(repo: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    let names = run_isolated(
        repo,
        &["diff", "--no-renames", "--name-only", "-z", from, to, "--"],
    )
    .await?;
    Ok(names
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The `paths` that are, contain or lie in a path of the unsaved files
/// `own`, as its worktree compares paths, since writing either would
/// replace the other.
fn overlap(paths: &[String], own: &Unsaved) -> Vec<String> {
    let collides = |path: &str| {
        (own.paths.iter()).any(|other| checkpoint::collides(path, other, own.ignores_case))
    };
    (paths.iter())
        .filter(|path| collides(path))
        .cloned()
        .collect()
}

/// The commits of `source` that `previous` lacks, oldest first.
async fn commits(repo: &Path, previous: &CommitId, source: &CommitId) -> Result<Vec<String>> {
    let range = format!("{previous}..{source}");
    let log = run_isolated(
        repo,
        &[
            "log",
            "--reverse",
            "--no-decorate",
            "--format=%h %s",
            &range,
            "--",
        ],
    )
    .await?;
    Ok(log.lines().map(str::to_owned).collect())
}

/// Write the files `paths` of `tree` into `worktree`, leaving its index
/// alone, and deleting those the tree lacks.
async fn write_files(worktree: &Path, tree: &str, paths: &[String]) -> Result<()> {
    let mut command = super::isolated_command_without_submodules(worktree);
    command
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(["restore", &format!("--source={tree}"), "--worktree"])
        .args(["--pathspec-from-file=-", "--pathspec-file-nul"]);
    super::bounded(command)
        .input(paths.join("\0"))
        .output()
        .await
        .context("write the carried files")
        .map(drop)
}

#[cfg(test)]
mod tests;
