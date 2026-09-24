//! Git invocations shared by workspace creation, removal, branch refreshes, and merges.
pub mod default_branch;
mod diff;
pub mod existing_branch;
pub mod merge;
pub mod repo;
pub mod worktrunk;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use crate::subprocess;

pub const LOCAL_REFS: &str = "refs/heads/";
pub const REMOTE_REFS: &str = "refs/remotes/";

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

/// `git -C <repo>` with the caller's normal configuration and hooks.
pub fn command(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo);
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

/// Ask Git whether `name` is acceptable branch syntax, independently of the
/// derived directory name. `repo` is the directory to ask from; `None` uses
/// the current one. Previous-checkout syntax such as `@{-1}` is rejected
/// because `--branch` would silently expand it; the historical `HEAD` name is
/// checked as its `HEAD-2` conflict spelling.
pub async fn check_branch_name(repo: Option<&Path>, name: &str) -> Result<()> {
    let checked = if name == "HEAD" { "HEAD-2" } else { name };
    let mut command = match repo {
        Some(repo) => self::command(repo),
        None => Command::new("git"),
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

pub async fn worktrees(repo: &Path) -> Result<Vec<Worktree>> {
    let listing = run(repo, &["worktree", "list", "--porcelain", "-z"]).await?;
    Ok(parse_worktrees(&listing))
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
