//! Branch names, renaming and checkout fast-forwards.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use shoal_refs::{BranchName, CommitId};

use crate::{fsutil::off_runtime, git, time::UnixTime};

/// Local and remote branch names that can conflict with a new local branch.
pub async fn names_in_use(repo: &Path) -> Result<Vec<BranchName>> {
    let refs = branch_refs(repo).await?;
    Ok(refs
        .iter()
        .filter_map(|reference| {
            git::strip_local(reference).or_else(|| {
                git::strip_remote(reference)?
                    .split_once('/')
                    .map(|(_, branch)| branch)
            })
        })
        .map(BranchName::from)
        .collect())
}

/// The selectors that open an existing branch, as `git switch` takes them:
/// every local branch, every remote one as `remote/branch`, and the bare
/// name of a remote branch that no local branch or other remote has.
pub async fn selectors(repo: &Path) -> Result<Vec<String>> {
    Ok(switch_selectors(&branch_refs(repo).await?))
}

fn switch_selectors(refs: &[String]) -> Vec<String> {
    let local: Vec<&str> = refs
        .iter()
        .filter_map(|name| git::strip_local(name))
        .collect();
    let remote: Vec<(&str, &str)> = (refs.iter())
        .filter_map(|reference| git::strip_remote(reference)?.split_once('/'))
        .filter(|(_, branch)| *branch != "HEAD")
        .collect();
    let unique = |name: &str| {
        !local.contains(&name) && remote.iter().filter(|(_, branch)| *branch == name).count() == 1
    };
    let bare = (remote.iter())
        .filter(|(_, branch)| unique(branch))
        .map(|(_, branch)| (*branch).to_owned());
    let qualified = (remote.iter()).map(|(remote, branch)| format!("{remote}/{branch}"));
    (local.iter().map(|name| (*name).to_owned()))
        .chain(qualified)
        .chain(bare)
        .collect()
}

async fn branch_refs(repo: &Path) -> Result<Vec<String>> {
    let refs = git::refs::list(repo, &[git::LOCAL_REFS, git::REMOTE_REFS]).await?;
    Ok(refs.into_iter().map(|reference| reference.name).collect())
}

/// Names Git reads as a revision rather than as a branch: `@` is HEAD's
/// shorthand, Git refuses a branch named `HEAD`, and a full object ID names a
/// commit. Both object-ID lengths are reserved whatever the repository's
/// format. The whole name counts, not its components: `topic/HEAD` and
/// `HEAD/topic` are literal branches.
pub fn is_reserved_name(name: &str) -> bool {
    matches!(name, "HEAD" | "@")
        || (matches!(name.len(), 40 | 64) && name.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// Refuse `name` unless it is a literal Git branch name, as `git
/// check-ref-format --branch` checks one, without starting Git. Revision
/// syntax such as `@{-1}` is refused rather than expanded, and the reserved
/// `HEAD` is accepted, since a new branch takes its `HEAD-2` conflict
/// spelling.
pub fn check_branch_name(name: &str) -> Result<()> {
    ensure!(
        name == "HEAD" || is_branch_name(name),
        "'{name}' is not a valid Git branch name; use a literal branch name"
    );
    Ok(())
}

/// Whether `refs/heads/<name>` is a ref name Git accepts and `name` does not
/// read as an option.
fn is_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("@{")
        && !name
            .bytes()
            .any(|byte| byte.is_ascii_control() || b" ~^:?*[\\".contains(&byte))
        && name.split('/').all(|component| {
            !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
        })
}

/// Create a local branch at `rev`; an existing branch is an error.
pub async fn create(repo: &Path, name: &BranchName, rev: &str) -> Result<String> {
    let args = ["branch", "--no-track", "--", name.as_str(), rev];
    git::run_isolated(repo, &args).await
}

/// Delete a local branch only while it still points at `commit`, so a
/// branch that moved meanwhile stays; a symbolic ref goes itself, never its
/// target. Its configuration goes too, as `git branch -D` removes it, so a
/// branch created later under the name does not inherit its upstream. The
/// caller checks that no worktree has it checked out.
pub async fn delete_at(repo: &Path, name: &BranchName, commit: &CommitId) -> Result<()> {
    git::run_isolated(
        repo,
        &[
            "update-ref",
            "--no-deref",
            "-d",
            &git::local_ref(name),
            commit.as_str(),
        ],
    )
    .await?;
    remove_config(repo, name).await
}

async fn remove_config(repo: &Path, name: &BranchName) -> Result<()> {
    let prefix = format!("branch.{name}.");
    let keys = git::run_isolated(repo, &["config", "--local", "--list", "--name-only"]).await?;
    // A longer branch name with this one as a dotted prefix has its own section.
    let configured = keys.lines().any(|key| {
        key.strip_prefix(&prefix)
            .is_some_and(|variable| !variable.contains('.'))
    });
    if configured {
        let section = format!("branch.{name}");
        git::run_isolated(repo, &["config", "--local", "--remove-section", &section]).await?;
    }
    Ok(())
}

/// Whether the local branch is a symbolic ref to another one.
pub async fn is_symbolic(repo: &Path, name: &BranchName) -> Result<bool> {
    let reference = git::local_ref(name);
    let listing = git::refs::list(repo, &[&reference]).await?;
    // The pattern also matches branches nested under the name.
    Ok(listing
        .iter()
        .any(|listed| listed.name == reference && listed.symref.is_some()))
}

/// Whether a rebase or bisect in some worktree started from the branch, or a
/// rebase will update it. Git shows that worktree's HEAD as detached, but the
/// operation returns to or rewrites the branch, so the branch is in use.
pub async fn in_operation(repo: &Path, name: &BranchName) -> Result<bool> {
    let common = git::common_dir(repo).await?;
    let name = name.clone();
    off_runtime::run(move || operation_on(common, &name)).await
}

fn operation_on(common: PathBuf, name: &BranchName) -> Result<bool> {
    let linked = match std::fs::read_dir(common.join("worktrees")) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let reference = git::local_ref(name);
    for dir in std::iter::once(common).chain(linked) {
        let read = |file: &str| {
            std::fs::read_to_string(dir.join(file)).map(|text| text.trim_end().to_owned())
        };
        // Rebases record full refs, `--update-refs` one per line among the
        // commits; bisects record the branch's short name.
        if read("rebase-merge/head-name").is_ok_and(|head| head == reference)
            || read("rebase-apply/head-name").is_ok_and(|head| head == reference)
            || read("rebase-merge/update-refs")
                .is_ok_and(|refs| refs.lines().any(|line| line == reference))
            || read("BISECT_START").is_ok_and(|start| start == *name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn commit_summary(repo: &Path, rev: &str) -> Result<(UnixTime, String)> {
    let output = git::run(repo, &["log", "-1", "--format=%ct%x00%s", rev, "--"]).await?;
    let (time, subject) = output
        .trim_end_matches('\n')
        .split_once('\0')
        .context("git log printed no commit summary")?;
    Ok((UnixTime::from_seconds(time.parse()?), subject.to_owned()))
}

pub async fn rename(repo: &Path, from: &BranchName, to: &BranchName) -> Result<String> {
    git::run_isolated(repo, &["branch", "-m", "--", from.as_str(), to.as_str()]).await
}

/// Fast-forward a checkout without recursive submodule updates.
pub async fn fast_forward(repo: &Path, rev: &str) -> Result<String> {
    git::run_without_submodules(
        repo,
        &[
            "merge",
            "--ff-only",
            "--no-edit",
            "--no-stat",
            "--no-overwrite-ignore",
            rev,
        ],
    )
    .await
}

#[cfg(test)]
mod tests;
