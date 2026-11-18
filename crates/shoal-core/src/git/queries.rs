//! Value queries with caller-selected Git invocation policy.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use shoal_refs::{BranchName, CommitId};

use super::{reader, refs, run};
use crate::fsutil::off_runtime;

/// Read an object or revision without peeling it to a commit.
pub async fn revision(
    repo: &Path,
    rev: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    named_object(repo, rev, async |repo| run(repo, &["rev-parse", rev]).await).await
}

/// Require one revision, without changing its object type.
pub async fn verify_revision(
    repo: &Path,
    rev: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    let git = async |repo: &Path| run(repo, &["rev-parse", "--verify", rev]).await;
    named_object(repo, rev, git).await
}

/// The object `rev` names, as `git rev-parse` prints it, read in process
/// for a ref or object, else by `git`.
async fn named_object(
    repo: &Path,
    rev: &str,
    git: impl AsyncFnOnce(&Path) -> Result<String>,
) -> Result<String> {
    if !refs::names_ref_or_object(rev) {
        return git(repo).await;
    }
    let owned = rev.to_owned();
    let in_process = move |repo: &gix::Repository| {
        let object = refs::named_object(repo, &owned)?;
        Ok(format!(
            "{}\n",
            object.with_context(|| format!("{owned} names nothing"))?
        ))
    };
    reader::read(repo, in_process, git).await
}

pub async fn symbolic_revision(
    repo: &Path,
    rev: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", "--symbolic-full-name", rev]).await
}

/// What HEAD names in a worktree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Head {
    /// The full name of the ref HEAD points to; `None` when detached.
    pub(crate) reference: Option<String>,
    /// HEAD's commit; `None` while its branch has none yet.
    pub(crate) commit: Option<CommitId>,
}

impl Head {
    /// The local branch HEAD is on; `None` when detached or on a ref
    /// outside `refs/heads/`.
    pub(crate) fn branch(&self) -> Option<BranchName> {
        let reference = self.reference.as_deref()?;
        super::strip_local(reference).map(BranchName::from)
    }

    /// HEAD's commit, which a branch without commits lacks.
    pub(crate) fn commit(self) -> Result<CommitId> {
        self.commit.context("HEAD names no commit yet")
    }
}

/// HEAD of the worktree Git finds from `repo`, its ref and commit read
/// together.
pub(crate) async fn head(repo: &Path) -> Result<Head> {
    reader::read(repo, head_in_process, head_from_git).await
}

/// The [`Head`] gix reads for `repo`.
fn head_in_process(repo: &gix::Repository) -> Result<Head> {
    let head = repo.head()?;
    Ok(Head {
        reference: head.referent_name().map(|name| name.as_bstr().to_string()),
        commit: head.id().map(|id| id.to_string().into()),
    })
}

/// The [`Head`] Git reads for `repo`.
async fn head_from_git(repo: &Path) -> Result<Head> {
    let mut reference = super::command(repo);
    reference.args(["symbolic-ref", "--quiet", "HEAD"]);
    let mut commit = super::command(repo);
    commit.args(["rev-parse", "--verify", "--quiet", "HEAD"]);
    // Both exit 1 for what HEAD lacks: a ref when detached, a commit when
    // its branch has none.
    let reference = super::output_unless(reference, 1).await?;
    let commit = super::output_unless(commit, 1).await?;
    Ok(Head {
        reference: reference.map(|name| name.trim_end().to_owned()),
        commit: commit.map(|commit| commit.trim().into()),
    })
}

/// HEAD's commit and the branch it is on, `None` when detached.
pub async fn head_and_branch(repo: &Path) -> Result<(CommitId, Option<BranchName>)> {
    let head = head(repo).await?;
    let branch = head.branch();
    Ok((head.commit()?, branch))
}

/// The commit HEAD names, its tree, and the branch it is on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HeadState {
    pub(crate) commit: CommitId,
    pub(crate) tree: String,
    /// `None` when HEAD is detached.
    pub(crate) branch: Option<BranchName>,
}

pub(crate) async fn head_state(repo: &Path) -> Result<HeadState> {
    let in_process = |repo: &gix::Repository| {
        let head = head_in_process(repo)?;
        Ok(HeadState {
            tree: repo.head_tree_id()?.to_string(),
            branch: head.branch(),
            commit: head.commit()?,
        })
    };
    reader::read(repo, in_process, head_state_from_git).await
}

/// The [`HeadState`] Git reads for `repo`.
async fn head_state_from_git(repo: &Path) -> Result<HeadState> {
    let output = run(repo, &["rev-parse", "HEAD^{tree}", "HEAD"]).await?;
    let mut lines = output.lines();
    let tree = lines.next().context("git rev-parse printed no tree")?;
    let commit = lines.next().context("git rev-parse printed no commit")?;
    Ok(HeadState {
        commit: commit.into(),
        tree: tree.to_owned(),
        branch: head_from_git(repo).await?.branch(),
    })
}

/// The branch the worktree at `repo` is on; `None` when detached.
pub async fn checked_out_branch(repo: &Path) -> Result<Option<BranchName>> {
    Ok(head(repo).await?.branch())
}

/// The canonical root of the working tree Git finds from `repo`.
pub async fn worktree_root(repo: &Path) -> Result<PathBuf> {
    let in_process = |repo: &gix::Repository| canonical(workdir(repo)?);
    let git = async |repo: &Path| {
        let root = run(repo, &["rev-parse", "--show-toplevel"]).await?;
        off_runtime::canonicalize(Path::new(root.trim_end_matches('\n'))).await
    };
    reader::read(repo, in_process, git).await
}

/// Whether the checkout at `path` has a `.git` directory, yet its Git config
/// sets `core.bare`, so Git refuses every working-tree command there. Running
/// `git init --bare` with an inherited `GIT_DIR` leaves a checkout this way.
pub(crate) async fn is_bare_with_work_tree(path: &Path) -> Result<bool> {
    if !off_runtime::is_dir(&path.join(".git")).await? {
        return Ok(false);
    }
    Ok(run(path, &["rev-parse", "--is-bare-repository"])
        .await?
        .trim()
        == "true")
}

/// The canonical Git directory of the checkout Git finds from `repo`: the
/// common one for a main checkout, its admin directory for a linked
/// worktree.
pub async fn git_dir(repo: &Path) -> Result<PathBuf> {
    let in_process = |repo: &gix::Repository| canonical(repo.git_dir());
    let git = async |repo: &Path| {
        let dir = run(repo, &["rev-parse", "--path-format=absolute", "--git-dir"]).await?;
        off_runtime::canonicalize(Path::new(dir.trim())).await
    };
    reader::read(repo, in_process, git).await
}

/// The canonical Git directory every worktree of the repository Git finds
/// from `repo` shares.
pub async fn common_dir(repo: &Path) -> Result<PathBuf> {
    let in_process = |repo: &gix::Repository| canonical(repo.common_dir());
    let git = async |repo: &Path| {
        let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
        let dir = run(repo, &args).await?;
        off_runtime::canonicalize(Path::new(dir.trim())).await
    };
    reader::read(repo, in_process, git).await
}

/// Where Git finds the checkout at a path, canonical, from one Git run.
#[derive(Debug, PartialEq, Eq)]
pub struct CheckoutDirs {
    /// The root of its working tree.
    pub root: PathBuf,
    /// The Git directory every worktree of the repository shares.
    pub common_dir: PathBuf,
    /// The checkout's own Git directory: the common one for a main
    /// checkout, its admin directory for a linked worktree.
    pub git_dir: PathBuf,
}

pub async fn checkout_dirs(repo: &Path) -> Result<CheckoutDirs> {
    let in_process = |repo: &gix::Repository| {
        Ok(CheckoutDirs {
            root: canonical(workdir(repo)?)?,
            common_dir: canonical(repo.common_dir())?,
            git_dir: canonical(repo.git_dir())?,
        })
    };
    reader::read(repo, in_process, checkout_dirs_from_git).await
}

/// The [`CheckoutDirs`] Git prints for `repo`.
async fn checkout_dirs_from_git(repo: &Path) -> Result<CheckoutDirs> {
    let output = run(
        repo,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
            "--git-dir",
        ],
    )
    .await?;
    let [root, common_dir, git_dir] = parse_checkout_dirs(&output)?;
    Ok(CheckoutDirs {
        root: off_runtime::canonicalize(&root).await?,
        common_dir: off_runtime::canonicalize(&common_dir).await?,
        git_dir: off_runtime::canonicalize(&git_dir).await?,
    })
}

/// The root of `repo`'s working tree, which a bare repository lacks, as Git
/// refuses `--show-toplevel` there.
fn workdir(repo: &gix::Repository) -> Result<&Path> {
    repo.workdir()
        .context("this operation must be run in a work tree")
}

/// `path` as Git prints a repository path: absolute, with symbolic links
/// resolved.
fn canonical(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).with_context(|| format!("cannot resolve {}", path.display()))
}

/// The three paths `rev-parse` prints, one per line, in the order asked.
fn parse_checkout_dirs(output: &str) -> Result<[PathBuf; 3]> {
    let mut lines = output.lines().map(PathBuf::from);
    let mut next = |what| {
        lines
            .next()
            .with_context(|| format!("git rev-parse printed no {what}"))
    };
    Ok([
        next("worktree root")?,
        next("common directory")?,
        next("Git directory")?,
    ])
}

/// The names Git keeps in the common directory, which every worktree
/// shares, rather than in each worktree's own Git directory.
const SHARED_GIT_PATHS: &[&str] = &[
    "branches",
    "common",
    "config",
    "gc.pid",
    "hooks",
    "info",
    "logs",
    "lost-found",
    "objects",
    "packed-refs",
    "refs",
    "remotes",
    "rr-cache",
    "shallow",
    "svn",
    "worktrees",
];

/// The absolute path of `name` in the Git directory of the worktree Git
/// finds from `repo`, as `git rev-parse --git-path` gives it. Shoal reads a
/// name of one component that Git keeps per worktree itself; Git places
/// every other.
pub(crate) async fn git_path(repo: &Path, name: &str) -> Result<PathBuf> {
    let git = async |repo: &Path| {
        let args = ["rev-parse", "--path-format=absolute", "--git-path", name];
        let path = run(repo, &args).await?;
        Ok(PathBuf::from(path.trim_end_matches('\n')))
    };
    if name.contains('/') || SHARED_GIT_PATHS.contains(&name) {
        return git(repo).await;
    }
    let owned = name.to_owned();
    let in_process = move |repo: &gix::Repository| Ok(repo.git_dir().join(&owned));
    reader::read(repo, in_process, git).await
}

/// The value Git gives `key` in the configuration of the repository Git
/// finds from `repo`, as `git config --get` reads it: the last one set;
/// `None` when it is unset or empty.
pub async fn config_value(repo: &Path, key: &str) -> Result<Option<String>> {
    let owned = key.to_owned();
    let in_process = move |repo: &gix::Repository| {
        let value = repo.config_snapshot().plumbing().string(owned.as_str());
        Ok(value.map(|value| value.to_string()))
    };
    let git = async |repo: &Path| {
        let value = run(repo, &["config", "--default", "", "--get", key]).await?;
        Ok(Some(value.trim_end_matches('\n').to_owned()))
    };
    let value = reader::read(repo, in_process, git).await?;
    Ok(value.filter(|value| !value.trim().is_empty()))
}

pub async fn latest_subject(repo: &Path) -> Result<String> {
    run(repo, &["log", "-1", "--format=%s"]).await
}

/// The commits `head` has that none of `excluded` has, newest first and at
/// most `limit` of them, each with its subject and body and the files it
/// changed, as `git log --stat` shows them.
pub async fn log_stat(repo: &Path, head: &str, excluded: &[&str], limit: usize) -> Result<String> {
    let limit = format!("--max-count={limit}");
    let excluded: Vec<String> = excluded.iter().map(|commit| format!("^{commit}")).collect();
    let mut args = vec![
        "log",
        "--stat",
        "--no-color",
        "--format=commit %H%n%n%B",
        &limit,
        head,
    ];
    args.extend(excluded.iter().map(String::as_str));
    args.push("--");
    run(repo, &args).await
}

#[cfg(test)]
mod tests;
