//! Value queries with caller-selected Git invocation policy.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::run;
use crate::fsutil::off_runtime;

/// Read an object or revision without peeling it to a commit.
pub async fn revision(
    repo: &Path,
    reference: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", reference]).await
}

/// Require one revision, without changing its object type.
pub async fn verify_revision(
    repo: &Path,
    reference: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", "--verify", reference]).await
}

pub async fn symbolic_revision(
    repo: &Path,
    reference: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", "--symbolic-full-name", reference]).await
}

/// HEAD's commit and the branch it is on, `None` when detached, read with
/// one Git process instead of two.
pub async fn head_and_branch(repo: &Path) -> Result<(String, Option<String>)> {
    let output = run(repo, &["rev-parse", "HEAD", "--symbolic-full-name", "HEAD"]).await?;
    parse_head_and_branch(&output)
}

/// Git prints `HEAD` as the name of a detached HEAD, and nothing when it
/// could not read the name, which leaves the branch unknown rather than
/// detached.
fn parse_head_and_branch(output: &str) -> Result<(String, Option<String>)> {
    let mut lines = output.lines();
    let commit = lines.next().context("git rev-parse printed no commit")?;
    let name = lines
        .next()
        .context("git rev-parse printed no name for HEAD")?;
    let branch = name.strip_prefix("refs/heads/");
    Ok((commit.to_owned(), branch.map(str::to_owned)))
}

/// Detached HEAD returns an empty branch name.
pub async fn current_branch(repo: &Path) -> Result<String> {
    run(repo, &["branch", "--show-current"]).await
}

pub async fn worktree_root(repo: &Path) -> Result<String> {
    run(repo, &["rev-parse", "--show-toplevel"]).await
}

/// Whether the checkout at `path` has a `.git` directory, yet its Git config
/// sets `core.bare`, so Git refuses every working-tree command there. Running
/// `git init --bare` with an inherited `GIT_DIR` leaves a checkout this way.
pub async fn is_bare_with_work_tree(path: &Path) -> Result<bool> {
    if !off_runtime::is_dir(&path.join(".git")).await? {
        return Ok(false);
    }
    Ok(run(path, &["rev-parse", "--is-bare-repository"])
        .await?
        .trim()
        == "true")
}

pub async fn git_dir(repo: &Path) -> Result<PathBuf> {
    let dir = run(repo, &["rev-parse", "--path-format=absolute", "--git-dir"]).await?;
    off_runtime::canonicalize(Path::new(dir.trim())).await
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

pub async fn metadata_path(
    repo: &Path,
    name: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["rev-parse", "--git-path", name]).await
}

pub async fn remotes(
    repo: &Path,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    run(repo, &["remote"]).await
}

/// The URLs the repository's remotes name, as configured, before any
/// rewriting.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RemoteUrls {
    /// Each `remote.<name>.url`, which pushes use unless a push URL is set.
    pub fetch: Vec<String>,
    /// Each `remote.<name>.pushurl`.
    pub push: Vec<String>,
}

pub async fn remote_urls(repo: &Path) -> Result<RemoteUrls> {
    let listed = run(repo, &["config", "--null", "--list"]).await?;
    let mut urls = RemoteUrls::default();
    for entry in listed.split('\0') {
        let Some((key, value)) = entry.split_once('\n') else {
            continue;
        };
        let Some(variable) = key
            .strip_prefix("remote.")
            .and_then(|rest| rest.rsplit_once('.'))
            .map(|(_, variable)| variable.to_ascii_lowercase())
        else {
            continue;
        };
        let list = match variable.as_str() {
            "url" => &mut urls.fetch,
            "pushurl" => &mut urls.push,
            _ => continue,
        };
        if !value.is_empty() && !list.iter().any(|url| url == value) {
            list.push(value.to_owned());
        }
    }
    Ok(urls)
}

pub async fn remote_url(repo: &Path, remote: &str) -> Result<String> {
    run(repo, &["remote", "get-url", remote]).await
}

/// The remote a push of `branch` goes to: `branch.<name>.pushRemote`, else
/// `remote.pushDefault`, else `origin`.
pub async fn push_remote(repo: &Path, branch: &str) -> Result<String> {
    for key in [
        format!("branch.{branch}.pushRemote"),
        "remote.pushDefault".into(),
    ] {
        let value = run(repo, &["config", "--default", "", "--get", &key]).await?;
        if !value.trim().is_empty() {
            return Ok(value.trim().to_owned());
        }
    }
    Ok("origin".into())
}

pub async fn latest_subject(repo: &Path) -> Result<String> {
    run(repo, &["log", "-1", "--format=%s"]).await
}

#[cfg(test)]
mod tests;
