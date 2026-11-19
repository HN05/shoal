//! Deleting a branch of a repository Shoal reaches by its URL, signed in as
//! an account, only while the branch points to the commit the caller
//! checked: Git sends that commit with the deletion, and the remote keeps a
//! branch that moved since, such as by a push that landed after the check.
use std::{io, path::Path, process::Output};

use anyhow::{Result, bail};
use shoal_forge::account::GitLogin;
use shoal_refs::{BranchName, CommitId};
use tokio::process::Command;

use crate::{fsutil::off_runtime, git, subprocess};

/// The credential helper that answers Git's `get` with the login the
/// environment holds, which keeps the token off the command line.
const HELPER: &str = "!f() { if [ \"$1\" = get ]; then \
    printf 'username=%s\\npassword=%s\\n' \"$SHOAL_GIT_USERNAME\" \"$SHOAL_GIT_PASSWORD\"; \
    fi; }; f";

/// Delete `branch` of the repository at `url` as `login` while it points to
/// `head`, or refuse it, deleting nothing, once it does not.
pub async fn delete_at(
    url: &str,
    branch: &BranchName,
    head: &CommitId,
    login: &GitLogin,
) -> Result<()> {
    let pushed = from_empty_repository(object_format(head), |empty| {
        deletion(empty, url, branch, head, login)
    })
    .await?;
    concluded(pushed, branch, head)
}

/// Run the push `push` builds for an empty repository, in one made for it
/// in the object format `format` and removed after, since Git pushes only
/// from a repository of the remote's format.
async fn from_empty_repository(
    format: &str,
    push: impl FnOnce(&Path) -> Command,
) -> Result<io::Result<Output>> {
    let empty = off_runtime::run(|| Ok(tempfile::tempdir()?)).await?;
    let format = format!("--object-format={format}");
    git::run_isolated(empty.path(), &["init", "--quiet", "--bare", &format]).await?;
    let pushed = git::bounded(push(empty.path())).capture().await;
    off_runtime::run(move || Ok(empty.close()?)).await?;
    Ok(pushed)
}

/// Git's name for the object format `head` is an ID in.
fn object_format(head: &CommitId) -> &'static str {
    match head.as_str().len() {
        64 => "sha256",
        _ => "sha1",
    }
}

/// The outcome of the deletion of `branch` at `head` that `pushed` ran.
fn concluded(pushed: io::Result<Output>, branch: &BranchName, head: &CommitId) -> Result<()> {
    let destination = git::local_ref(branch);
    let reported = (pushed.as_ref().ok())
        .and_then(|output| reported(&String::from_utf8_lossy(&output.stdout), &destination));
    match reported {
        Some(Report::Deleted) => Ok(()),
        Some(Report::Moved) => {
            bail!("branch {branch} moved from {head} since it was checked; list the branches again")
        }
        Some(Report::Refused(reason)) => bail!("the forge kept branch {branch}: {reason}"),
        None => {
            subprocess::checked_output("git push", pushed)?;
            bail!("git push did not report branch {branch}")
        }
    }
}

/// The push from the empty repository at `empty` that deletes `branch` at
/// `url` as `login` while it points to `head`, with neither the user's Git
/// configuration, which could send it elsewhere or as someone else, nor
/// any other credential helper.
fn deletion(
    empty: &Path,
    url: &str,
    branch: &BranchName,
    head: &CommitId,
    login: &GitLogin,
) -> Command {
    let destination = git::local_ref(branch);
    let mut command = git::isolated_command(empty);
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("SHOAL_GIT_USERNAME", login.username)
        .env("SHOAL_GIT_PASSWORD", &login.password)
        .args(["-c", "credential.helper=", "-c"])
        .arg(format!("credential.helper={HELPER}"))
        .args(git::porcelain_push_args(
            url,
            &[format!(":{destination}")],
            &[(&destination, head.as_str())],
        ));
    command
}

/// What Git reported of a deletion.
#[derive(Debug, PartialEq)]
enum Report {
    Deleted,
    /// The branch no longer pointed to the commit the deletion expected.
    Moved,
    /// The remote refused it, for the reason Git gives.
    Refused(String),
}

/// What `git push --porcelain` printed in `porcelain` of the deletion of
/// `destination`, if anything.
fn reported(porcelain: &str, destination: &str) -> Option<Report> {
    porcelain.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let (flag, refs) = (fields.next()?, fields.next()?);
        if refs.split_once(':')?.1 != destination {
            return None;
        }
        let summary = fields.next().unwrap_or_default();
        Some(match flag {
            "-" => Report::Deleted,
            _ if summary.contains("stale info") => Report::Moved,
            _ => Report::Refused(summary.to_owned()),
        })
    })
}

#[cfg(test)]
mod tests;
