//! Git invocations shared by workspace creation, removal, branch refreshes, and landing.
pub mod branch;
pub mod bundle;
pub mod config;
pub(crate) mod conflicts;
pub mod default_branch;
pub mod diff;
pub mod existing_branch;
pub mod fetch;
pub mod files;
mod predicates;
pub use predicates::{has_commit, is_ancestor, ref_exists};
mod queries;
pub use queries::{
    current_branch, git_dir, is_bare_with_work_tree, latest_subject, metadata_path, push_remote,
    remote_url, remotes, revision, symbolic_revision, verify_revision, worktree_root,
};
pub mod checkpoint;
pub mod repo;
pub mod seed;
pub mod status;
pub mod worktrunk;

use crate::tools::Tool;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use crate::subprocess;

pub const LOCAL_REFS: &str = "refs/heads/";
pub const REMOTE_REFS: &str = "refs/remotes/";

pub const FETCH_SAFE_ARGS: &[&str] = &[
    "fetch",
    "--no-tags",
    "--no-recurse-submodules",
    "--no-write-fetch-head",
];

/// Spell a literal local branch as a full ref, without validating its name.
pub fn local_ref(name: &str) -> String {
    format!("{LOCAL_REFS}{name}")
}

pub fn strip_local(reference: &str) -> Option<&str> {
    reference.strip_prefix(LOCAL_REFS)
}

/// An empty name gives the remote's ref prefix, including its trailing slash.
pub fn remote_ref(remote: &str, name: &str) -> String {
    format!("{REMOTE_REFS}{remote}/{name}")
}

/// Keep the remote qualification intact: remote names may contain slashes.
pub fn strip_remote(reference: &str) -> Option<&str> {
    reference.strip_prefix(REMOTE_REFS)
}

/// The configured remote and branch a base such as `origin/feature` names,
/// including a branch never fetched. Revision expressions, `HEAD`, and local
/// branches or tags that Git would resolve first are not remote branches.
/// The longest remote wins, since remote names may contain slashes.
pub async fn remote_branch(repo: &Path, base: &str) -> Result<Option<(String, String)>> {
    let short = base.strip_prefix("remotes/");
    let qualified = strip_remote(base).or(short);
    if qualified.is_none() {
        for prefix in [LOCAL_REFS, "refs/tags/"] {
            let shadow = format!("{prefix}{base}");
            if ref_exists(repo, &shadow, isolated_command).await? {
                return Ok(None);
            }
        }
    }
    let qualified = qualified.unwrap_or(base);
    if qualified.contains(['~', '^', ':', '?', '*', '[', '\\', ' ']) || qualified.contains("@{") {
        return Ok(None);
    }
    let remotes = run_isolated(repo, &["remote"]).await?;
    let mut remotes: Vec<&str> = remotes.lines().collect();
    remotes.sort_by_key(|remote| std::cmp::Reverse(remote.len()));
    Ok(remotes.into_iter().find_map(|remote| {
        let branch = qualified.strip_prefix(&format!("{remote}/"))?;
        (!branch.is_empty() && branch != "HEAD").then(|| (remote.to_owned(), branch.to_owned()))
    }))
}

/// A branch a remote advertises, with its commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHead {
    /// The full ref on the remote, such as `refs/heads/main`.
    pub name: String,
    pub commit: String,
}

/// The branches `remote` itself advertises now, whatever the local
/// remote-tracking refs say.
pub async fn remote_heads(repo: &Path, remote: &str) -> Result<Vec<RemoteHead>> {
    let listing = run_isolated(repo, &["ls-remote", "--heads", "--", remote]).await?;
    Ok(listing
        .lines()
        .filter_map(|line| {
            let (commit, name) = line.split_once('\t')?;
            Some(RemoteHead {
                name: name.to_owned(),
                commit: commit.to_owned(),
            })
        })
        .collect())
}

/// The heads whose commits this repository has, in one Git process.
pub async fn present_heads(repo: &Path, heads: &[RemoteHead]) -> Result<Vec<RemoteHead>> {
    if heads.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["rev-list", "--no-walk", "--ignore-missing"];
    args.extend(heads.iter().map(|head| head.commit.as_str()));
    let present = run_isolated(repo, &args).await?;
    let present: Vec<&str> = present.lines().collect();
    Ok(heads
        .iter()
        .filter(|head| present.contains(&head.commit.as_str()))
        .cloned()
        .collect())
}

/// Whether `remote` itself advertises `branch`, which need not be fetched yet.
pub async fn remote_has_branch(repo: &Path, remote: &str, branch: &str) -> Result<bool> {
    let heads = run_isolated(
        repo,
        &["ls-remote", "--heads", "--", remote, &local_ref(branch)],
    )
    .await?;
    Ok(!heads.trim().is_empty())
}

/// The commit `remote`'s push destination has for `branch`: what a push from
/// this repository updated, even where it differs from the fetch URL.
pub async fn pushed_commit(repo: &Path, remote: &str, branch: &str) -> Result<Option<String>> {
    let url = run_isolated(repo, &["remote", "get-url", "--push", remote]).await?;
    let heads = run_isolated(
        repo,
        &["ls-remote", "--heads", "--", url.trim(), &local_ref(branch)],
    )
    .await?;
    Ok(heads
        .split_once('\t')
        .map(|(commit, _)| commit.trim().to_owned()))
}

/// `git -C <repo>` with the caller's normal configuration and hooks. Queries
/// skip optional locks, so a status that refreshes the index never holds the
/// lock a concurrent Git command in the same worktree needs.
pub fn command(repo: &Path) -> Command {
    let mut command = Command::new(Tool::Git.program());
    command.arg("--no-optional-locks").arg("-C").arg(repo);
    command
}

/// Git for daemon-driven ref updates with repository hooks, Git credential
/// prompts, and SSH askpass disabled.
pub fn isolated_command(repo: &Path) -> Command {
    let mut command = command(repo);
    command
        .args(["-c", "core.hooksPath=/dev/null"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "never");
    command
}

/// Isolated Git with recursive submodule operations disabled.
pub fn isolated_command_without_submodules(repo: &Path) -> Command {
    let mut command = isolated_command(repo);
    command.args(["-c", "submodule.recurse=false"]);
    command
}

pub async fn run_without_submodules(repo: &Path, args: &[&str]) -> Result<String> {
    let mut command = isolated_command_without_submodules(repo);
    command.args(args);
    subprocess::output(command).await
}

/// Merge a resolved source, allowing a fast-forward or a named merge commit.
pub fn merge_commit(repo: &Path, branch: &str, commit: &str) -> Command {
    let mut command = isolated_command_without_submodules(repo);
    command.args([
        "merge",
        "--ff",
        "--no-squash",
        "--no-edit",
        "--no-stat",
        "--no-autostash",
        "--no-overwrite-ignore",
        "-m",
        &format!("Merge branch '{branch}'"),
        "--",
        commit,
    ]);
    command
}

/// Native fetch refuses a checked-out destination and a non-fast-forward,
/// including a branch that became checked out since discovery.
pub async fn fast_forward_local(repo: &Path, commit: &str, reference: &str) -> Result<String> {
    run_isolated(
        repo,
        &[FETCH_SAFE_ARGS, &[".", &format!("{commit}:{reference}")]].concat(),
    )
    .await
}

pub async fn run(repo: &Path, args: &[&str]) -> Result<String> {
    let mut command = command(repo);
    command.args(args);
    subprocess::output(command).await
}

pub async fn run_isolated(repo: &Path, args: &[&str]) -> Result<String> {
    let mut command = isolated_command(repo);
    command.args(args);
    subprocess::output(command).await
}

/// Build the tracked command used by `shoal diff`, preserving the caller's
/// Git configuration, pager, and external diff settings.
pub fn diff_command(commit: &str) -> Vec<OsString> {
    [Tool::Git.program(), "diff", commit, "--"]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// Build the tracked command used when opening a pull request for a workspace.
pub fn push_command(remote: &str, branch: &str) -> Vec<OsString> {
    let branch = local_ref(branch);
    [
        Tool::Git.program(),
        "push",
        "--set-upstream",
        "--",
        remote,
        &format!("{branch}:{branch}"),
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// The remote a contribution clone pushes its branches to.
pub const FORK_REMOTE: &str = "fork";

/// Clone with the caller's normal Git configuration and hooks. With `fork`,
/// the clone pushes to that URL as the `fork` remote, whose fetch refspec
/// lets a push record the branch as pushed.
pub async fn clone(source: &str, destination: &Path, fork: Option<&str>) -> Result<()> {
    subprocess::output(clone_command(source, destination, fork)).await?;
    Ok(())
}

/// The command [`clone`] runs.
fn clone_command(source: &str, destination: &Path, fork: Option<&str>) -> Command {
    let mut command = Command::new(Tool::Git.program());
    command.arg("clone");
    if let Some(url) = fork {
        for setting in [
            format!("remote.{FORK_REMOTE}.url={url}"),
            format!("remote.{FORK_REMOTE}.fetch=+refs/heads/*:refs/remotes/{FORK_REMOTE}/*"),
            format!("remote.pushDefault={FORK_REMOTE}"),
        ] {
            command.arg("-c").arg(setting);
        }
    }
    command.arg("--").arg(source).arg(destination);
    command
}

/// The local branch named by HEAD. Detached HEAD remains a Git error; a
/// symbolic HEAD outside the local namespace returns `None`. Pass `run` or
/// `run_isolated` (or the caller's runner) to keep invocation policy explicit.
pub async fn head_branch(
    repo: &Path,
    quiet: bool,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<Option<String>> {
    let args: &[&str] = if quiet {
        &["symbolic-ref", "--quiet", "HEAD"]
    } else {
        &["symbolic-ref", "HEAD"]
    };
    let head = run(repo, args).await?;
    Ok(strip_local(head.trim_end_matches('\n')).map(str::to_owned))
}

/// Resolve and peel a revision to a commit, using the caller's Git runner.
/// Plain ref/object queries must not use this: `^{commit}` changes their meaning.
pub async fn resolve_commit(
    repo: &Path,
    reference: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<String> {
    Ok(run(
        repo,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )
    .await?
    .trim()
    .to_owned())
}

/// Ask Git whether `name` is acceptable branch syntax, independently of the
/// derived directory name. `repo` is the directory to ask from; `None` uses
/// the current one. Previous-checkout syntax such as `@{-1}` is rejected
/// because `--branch` would silently expand it; the historical `HEAD` name is
/// checked as its `HEAD-2` conflict spelling.
pub async fn check_branch_name(repo: Option<&Path>, name: &str) -> Result<()> {
    let checked = if name == "HEAD" { "HEAD-2" } else { name };
    let mut command = match repo {
        Some(repo) => self::command(repo),
        None => Command::new(Tool::Git.program()),
    };
    command.args(["check-ref-format", "--branch", checked]);
    let validated = subprocess::output(command)
        .await
        .context("invalid Git branch name")?;
    ensure!(
        validated == format!("{checked}\n"),
        "use a literal Git branch name, not previous-checkout syntax"
    );
    Ok(())
}

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
    pub fn is_branch(&self, name: &str) -> bool {
        self.branch.as_deref() == Some(&local_ref(name))
    }
}

/// The canonical Git directory shared by every worktree of the checkout at `path`.
pub async fn common_dir(path: &Path) -> Result<PathBuf> {
    let dir = run(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    Ok(std::fs::canonicalize(dir.trim())?)
}

pub async fn worktrees(
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
    branch: &str,
    run: impl AsyncFn(&Path, &[&str]) -> Result<String>,
) -> Result<Option<Worktree>> {
    Ok(worktrees(repo, run)
        .await?
        .into_iter()
        .find(|tree| tree.is_branch(branch)))
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_clone_with_a_fork_pushes_there_and_counts_the_push() {
        use crate::test_support::{commit, git, repository};
        let root = tempfile::tempdir().unwrap();
        let upstream = repository(root.path(), "upstream");
        let fork = root.path().join("fork.git");
        git(
            root.path(),
            &[
                "clone",
                "--bare",
                "--",
                upstream.to_str().unwrap(),
                fork.to_str().unwrap(),
            ],
        );
        let clone_path = root.path().join("clone");
        // The user's Git configuration and hooks stay out of the test.
        let mut cloning = clone_command(
            upstream.to_str().unwrap(),
            &clone_path,
            Some(fork.to_str().unwrap()),
        );
        cloning
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        subprocess::output(cloning).await.unwrap();
        assert_eq!(
            push_remote(&clone_path, "topic").await.unwrap(),
            FORK_REMOTE
        );
        git(&clone_path, &["switch", "-c", "topic"]);
        std::fs::write(clone_path.join("change"), "change\n").unwrap();
        commit(&clone_path, "change");
        assert_eq!(
            status::unretained_commits(&clone_path, "HEAD", None)
                .await
                .unwrap(),
            1
        );
        let push = push_command(FORK_REMOTE, "topic");
        let args: Vec<&str> = push[1..].iter().map(|arg| arg.to_str().unwrap()).collect();
        git(&clone_path, &args);
        assert_eq!(
            status::unretained_commits(&clone_path, "HEAD", None)
                .await
                .unwrap(),
            0
        );
        assert!(
            !remote_has_branch(&clone_path, "origin", "topic")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn value_queries_keep_arguments_and_runner_errors() {
        let repo = Path::new("/repo");
        for quiet in [false, true] {
            let branch = head_branch(repo, quiet, async |path, args| {
                assert_eq!(path, repo);
                let expected = if quiet {
                    vec!["symbolic-ref", "--quiet", "HEAD"]
                } else {
                    vec!["symbolic-ref", "HEAD"]
                };
                assert_eq!(args, expected);
                Ok("refs/heads/feature/nested\n".into())
            })
            .await
            .unwrap();
            assert_eq!(branch.as_deref(), Some("feature/nested"));
        }
        let commit = resolve_commit(repo, "refs/tags/v1", async |path, args| {
            assert_eq!(path, repo);
            assert_eq!(args, ["rev-parse", "--verify", "refs/tags/v1^{commit}"]);
            Ok("abc\n".into())
        })
        .await
        .unwrap();
        assert_eq!(commit, "abc");
        let tree = checkout_of(repo, "feature/nested", async |path, args| {
            assert_eq!(path, repo);
            assert_eq!(args, ["worktree", "list", "--porcelain", "-z"]);
            Ok("worktree /detached\0detached\0\0worktree /feature\0branch refs/heads/feature/nested\0locked\0prunable\0\0".into())
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(tree.path, Path::new("/feature"));
        assert!(tree.locked && tree.prunable);

        async fn failed(_: &Path, _: &[&str]) -> Result<String> {
            anyhow::bail!("runner failed")
        }
        assert_eq!(
            head_branch(repo, true, failed)
                .await
                .unwrap_err()
                .to_string(),
            "runner failed"
        );
        assert_eq!(
            resolve_commit(repo, "HEAD", failed)
                .await
                .unwrap_err()
                .to_string(),
            "runner failed"
        );
        assert_eq!(
            checkout_of(repo, "main", failed)
                .await
                .unwrap_err()
                .to_string(),
            "runner failed"
        );
    }

    #[tokio::test]
    async fn value_queries_distinguish_branches_tags_and_detached_head() {
        use crate::test_support::{git, repository};
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path(), "repo");
        git(&repo, &["branch", "-m", "feature/nested"]);
        git(&repo, &["tag", "-a", "v1", "-m", "annotated"]);
        let expected = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
        assert_eq!(
            resolve_commit(&repo, "refs/tags/v1", run).await.unwrap(),
            expected
        );
        assert_eq!(
            resolve_commit(&repo, &local_ref("feature/nested"), run_isolated)
                .await
                .unwrap(),
            expected
        );
        assert!(resolve_commit(&repo, "HEAD^{tree}", run).await.is_err());
        assert!(
            resolve_commit(&repo, "missing", run_isolated)
                .await
                .is_err()
        );
        assert_eq!(
            head_branch(&repo, true, run).await.unwrap().as_deref(),
            Some("feature/nested")
        );
        assert_eq!(
            head_branch(&repo, false, run_isolated)
                .await
                .unwrap()
                .as_deref(),
            Some("feature/nested")
        );
        assert_eq!(
            checkout_of(&repo, "feature/nested", run)
                .await
                .unwrap()
                .unwrap()
                .path,
            repo.canonicalize().unwrap()
        );
        assert!(checkout_of(&repo, "feature", run).await.unwrap().is_none());
        git(&repo, &["checkout", "--detach"]);
        assert!(head_branch(&repo, true, run).await.is_err());
        assert!(
            checkout_of(&repo, "feature/nested", run)
                .await
                .unwrap()
                .is_none()
        );
        git(&repo, &["symbolic-ref", "HEAD", "refs/heads/unborn"]);
        assert_eq!(
            head_branch(&repo, true, run).await.unwrap().as_deref(),
            Some("unborn")
        );
        git(&repo, &["symbolic-ref", "HEAD", "refs/tags/v1"]);
        assert_eq!(head_branch(&repo, true, run).await.unwrap(), None);
    }

    #[tokio::test]
    async fn background_ssh_disables_prompts_and_preserves_transport_selection() {
        let root = tempfile::tempdir().unwrap();
        let ssh = root.path().join("fake-ssh");
        std::fs::write(
            &ssh,
            r#"#!/bin/sh
printf 'transport=%s askpass=%s terminal=%s\n' "$1" "$SSH_ASKPASS_REQUIRE" "$GIT_TERMINAL_PROMPT" >&2
exit 1
"#,
        )
        .unwrap();
        for isolated in [false, true] {
            for override_transport in [false, true] {
                let mut command = if isolated {
                    isolated_command(root.path())
                } else {
                    let mut command = command(root.path());
                    command
                        .env("SSH_ASKPASS_REQUIRE", "force")
                        .env("GIT_TERMINAL_PROMPT", "1");
                    command
                };
                command
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_SSH_VARIANT", "ssh")
                    .env("SHOAL_TEST_SSH", &ssh)
                    .env_remove("GIT_SSH_COMMAND")
                    .args([
                        "-c",
                        "core.sshCommand=sh \"$SHOAL_TEST_SSH\" config",
                        "ls-remote",
                        "git@example.invalid:team/project.git",
                    ]);
                if override_transport {
                    command.env("GIT_SSH_COMMAND", "sh \"$SHOAL_TEST_SSH\" env");
                }
                let error = subprocess::output(command).await.unwrap_err();
                let transport = if override_transport { "env" } else { "config" };
                let prompts = if isolated {
                    "askpass=never terminal=0"
                } else {
                    "askpass=force terminal=1"
                };
                assert!(
                    error
                        .to_string()
                        .contains(&format!("transport={transport} {prompts}")),
                    "{error:#}"
                );
            }
        }
    }

    #[test]
    fn parses_porcelain_worktree_records() {
        let listing = "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0\
            worktree /repo/.wt/feature\0HEAD def\0branch refs/heads/feature\0locked reason\0prunable gitdir file points to non-existent location\0\0\
            worktree /repo/.wt/detached\0HEAD 123\0detached\0\0";
        let trees = parse_worktrees(listing);
        assert_eq!(trees.len(), 3);
        assert!(trees[0].is_branch("main") && !trees[0].locked && !trees[0].prunable);
        assert_eq!(trees[1].path, PathBuf::from("/repo/.wt/feature"));
        assert!(trees[1].locked && trees[1].prunable);
        assert_eq!(trees[2].branch, None);
        assert!(parse_worktrees("").is_empty());
    }

    #[tokio::test]
    async fn branch_names_follow_git_syntax() {
        for name in ["feature/login", "fix-2", "HEAD", "release/v1.0.0"] {
            check_branch_name(None, name).await.unwrap();
        }
        for name in [
            "",
            "bad name",
            "-dash",
            "a..b",
            "trailing/",
            "@{-1}",
            "refs/heads/x\n",
        ] {
            let error = check_branch_name(None, name).await.unwrap_err();
            assert!(
                format!("{error:#}").contains("branch name"),
                "{name:?}: {error:#}"
            );
        }
    }
}
