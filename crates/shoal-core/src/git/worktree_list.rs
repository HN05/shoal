//! The worktrees a repository registers, as `git worktree list --porcelain`
//! lists them, read in process from the repository's admin directories
//! where gix reads the repository as Git does.
use std::path::{Path, PathBuf};

use anyhow::Result;
use shoal_refs::BranchName;

use super::{local_ref, reader, refs};

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Full ref name such as `refs/heads/main`; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Git considers the registration removable because its directory is gone.
    pub prunable: bool,
    pub locked: bool,
}

impl Worktree {
    pub fn is_branch(&self, name: &BranchName) -> bool {
        self.branch.as_deref() == Some(&local_ref(name))
    }
}

/// The worktrees of the repository Git finds from `repo`, the main checkout
/// first, as `git worktree list` gives them; `run` lists them where gix
/// would not read the repository as Git does.
pub async fn worktrees(
    repo: &Path,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<Vec<Worktree>> {
    let git = async |repo: &Path| listed_worktrees(repo, run).await;
    reader::read_unless_deferred(repo, |repo| Ok(in_process(repo)), git).await
}

/// The worktrees `git worktree list`, run by `run`, lists for the
/// repository Git finds from `repo`: what a removal or cleanup decision
/// reads, where a worktree missed could cost the work on its branch.
pub async fn listed_worktrees(
    repo: &Path,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<Vec<Worktree>> {
    let listing = run(repo, &["worktree", "list", "--porcelain", "-z"]).await?;
    Ok(parse_worktrees(&listing))
}

/// The first worktree registered on this literal local branch, including
/// locked or prunable entries. The caller decides whether it is usable.
pub async fn checkout_of(
    repo: &Path,
    branch: &BranchName,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<Option<Worktree>> {
    Ok(on_branch(worktrees(repo, run).await?, branch))
}

/// [`checkout_of`] as [`listed_worktrees`] lists them, for a removal or
/// cleanup decision.
pub async fn listed_checkout_of(
    repo: &Path,
    branch: &BranchName,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<Option<Worktree>> {
    Ok(on_branch(listed_worktrees(repo, run).await?, branch))
}

/// The first of `worktrees` on the literal local `branch`.
fn on_branch(worktrees: Vec<Worktree>, branch: &BranchName) -> Option<Worktree> {
    worktrees.into_iter().find(|tree| tree.is_branch(branch))
}

/// The worktrees of `repo` as Git lists them; `None` where a worktree's
/// configuration, path or HEAD may read differently to Git.
fn in_process(repo: &gix::Repository) -> Option<Vec<Worktree>> {
    let config = repo.config_snapshot();
    // Each worktree may then set `core.bare` in a file of its own.
    if config.boolean("extensions.worktreeConfig") == Some(true) {
        return None;
    }
    let common = std::fs::canonicalize(repo.common_dir()).ok()?;
    let main = main_worktree(repo, &config, &common)?;
    let mut linked = linked_worktrees(repo, &common)?;
    let ignore_case = config.boolean("core.ignoreCase") == Some(true);
    linked.sort_by_cached_key(|(path, _)| sort_key(path, ignore_case));
    let linked = linked.into_iter().map(|(_, worktree)| worktree);
    Some(std::iter::once(main).chain(linked).collect())
}

/// The linked worktrees the admin directories under `common` record, each
/// with the path it sorts by, in no order.
fn linked_worktrees(repo: &gix::Repository, common: &Path) -> Option<Vec<(String, Worktree)>> {
    // Git lists none where it cannot read the directory.
    let admin_dirs = std::fs::read_dir(common.join("worktrees"))
        .into_iter()
        .flatten();
    let mut linked = Vec::new();
    for admin_dir in admin_dirs {
        linked.extend(linked_worktree(repo, &admin_dir.ok()?.path())?);
    }
    Some(linked)
}

/// The main checkout's entry: the common directory's path without `/.git`,
/// on no branch when Git takes the repository as bare.
fn main_worktree(
    repo: &gix::Repository,
    config: &gix::config::Snapshot<'_>,
    common: &Path,
) -> Option<Worktree> {
    let common = common.to_str()?;
    let path = common.strip_suffix("/.git").unwrap_or(common);
    let opened_main = repo.git_dir() == repo.common_dir();
    let bare = match config.boolean("core.bare") {
        Some(bare) => bare,
        None => opened_main && repo.workdir().is_none(),
    };
    let branch = if bare {
        None
    } else {
        head_branch(repo, "main-worktree/HEAD")?
    };
    Some(Worktree {
        path: PathBuf::from(path),
        branch,
        prunable: false,
        locked: false,
    })
}

/// The linked worktree whose admin directory is `admin_dir`, with the path
/// it sorts by; `Some(None)` for one Git leaves out of its list, as it
/// does where its `gitdir` file is missing or empty.
fn linked_worktree(repo: &gix::Repository, admin_dir: &Path) -> Option<Option<(String, Worktree)>> {
    let Ok(gitdir) = std::fs::read(admin_dir.join("gitdir")) else {
        return Some(None);
    };
    if gitdir.is_empty() {
        return Some(None);
    }
    let gitdir = String::from_utf8(gitdir).ok()?;
    // Git lists the path without trailing whitespace, and tests for
    // pruning the one without trailing line ends.
    let named = gitdir.trim_end();
    let dotgit = gitdir.trim_end_matches(['\n', '\r']);
    if named.is_empty() {
        return None;
    }
    let path = absolute(admin_dir, named.strip_suffix("/.git").unwrap_or(named));
    let path = path.to_str()?.to_owned();
    let id = admin_dir.file_name()?.to_str()?;
    let branch = head_branch(repo, &format!("worktrees/{id}/HEAD"))?;
    let locked = admin_dir.join("locked").exists();
    let worktree = Worktree {
        path: PathBuf::from(&path),
        branch,
        prunable: !locked && !absolute(admin_dir, dotgit).exists(),
        locked,
    };
    Some(Some((path, worktree)))
}

/// `named`, which an admin directory's `gitdir` file holds: as written
/// when absolute, else resolved from `admin_dir` as far as it exists.
fn absolute(admin_dir: &Path, named: &str) -> PathBuf {
    if Path::new(named).is_absolute() {
        return PathBuf::from(named);
    }
    let admin_dir = std::fs::canonicalize(admin_dir).unwrap_or_else(|_| admin_dir.to_owned());
    resolved(&admin_dir.join(named))
}

/// `path` with symbolic links resolved as far as it exists.
fn resolved(path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => resolved(parent).join(name),
        _ => path.to_owned(),
    }
}

/// The branch the per-worktree `head` names, followed as Git follows it:
/// `Some(None)` when detached or missing, as Git then lists no branch;
/// `None` where gix fails to read it.
fn head_branch(repo: &gix::Repository, head: &str) -> Option<Option<String>> {
    Some(refs::followed(repo, head).ok()?.target)
}

/// What Git sorts linked worktrees by: their path's bytes, compared without
/// ASCII case where the repository ignores case.
fn sort_key(path: &str, ignore_case: bool) -> Vec<u8> {
    match ignore_case {
        true => path.to_ascii_lowercase().into_bytes(),
        false => path.as_bytes().to_owned(),
    }
}

fn parse_worktrees(listing: &str) -> Vec<Worktree> {
    listing
        .split("\0\0")
        .filter_map(|record| {
            let fields: Vec<&str> = record.split('\0').collect();
            let path = fields.iter().find_map(|f| f.strip_prefix("worktree "))?;
            Some(Worktree {
                path: PathBuf::from(path),
                branch: fields
                    .iter()
                    .find_map(|f| f.strip_prefix("branch "))
                    .map(str::to_owned),
                prunable: fields
                    .iter()
                    .any(|f| *f == "prunable" || f.starts_with("prunable ")),
                locked: fields
                    .iter()
                    .any(|f| *f == "locked" || f.starts_with("locked ")),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
