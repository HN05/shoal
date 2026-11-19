//! Deleting a branch of a repository Shoal reaches by its URL, signed in as
//! an account, only while the branch points to the commit the caller
//! checked: Git sends that commit with the deletion, and the remote keeps a
//! branch that moved since, such as by a push that landed after the check.
//!
//! The push runs from an empty repository made for it, without the user's
//! Git configuration, which could send it elsewhere or as someone else. Its
//! login reaches Git only on standard input, held by Git's in-memory
//! credential cache, which answers only for the repository's host, so the
//! token never enters a file, the environment or argv.
use std::{io, os::unix::fs::PermissionsExt as _, path::PathBuf, process::Output};

use anyhow::{Context, Result, bail, ensure};
use shoal_forge::account::GitLogin;
use shoal_refs::{BranchName, CommitId};
use tokio::process::Command;

use crate::{fsutil::off_runtime, git, subprocess};

/// Delete `branch` of the repository at `url` as `login` while it points to
/// `head`, or refuse it, deleting nothing, once it does not.
pub async fn delete_at(
    url: &str,
    branch: &BranchName,
    head: &CommitId,
    login: &GitLogin,
) -> Result<()> {
    let scratch = Scratch::create(url, head).await?;
    let pushed = scratch.delete(login, branch, head).await;
    let closed = scratch.close().await;
    let pushed = pushed?;
    closed?;
    concluded(pushed, branch, head)
}

/// The empty repository a deletion pushes from, with the credential cache
/// that holds its login, in a directory only this user reads, as the cache
/// requires of its socket's.
struct Scratch {
    directory: tempfile::TempDir,
    /// The URL the push goes to.
    url: String,
    /// The scheme and host of `url`, which the login is for.
    scheme: String,
    host: String,
}

impl Scratch {
    /// The repository for pushing to `url`, in the object format of `head`.
    async fn create(url: &str, head: &CommitId) -> Result<Self> {
        let (scheme, host) = scheme_and_host(url)?;
        let scratch = Self {
            directory: private_directory().await?,
            url: url.to_owned(),
            scheme: scheme.to_owned(),
            host: host.to_owned(),
        };
        scratch.init(head).await?;
        Ok(scratch)
    }

    /// Create the empty repository in the object format of `head`.
    async fn init(&self, head: &CommitId) -> Result<()> {
        let format = format!("--object-format={}", object_format(head));
        let repository = self.repository();
        let repository = repository.to_string_lossy();
        let init = ["init", "--quiet", "--bare", &format, &repository];
        git::run_isolated(self.directory.path(), &init).await?;
        Ok(())
    }

    /// Push the deletion of `branch` at `head` as `login`, with what Git
    /// printed.
    async fn delete(
        &self,
        login: &GitLogin,
        branch: &BranchName,
        head: &CommitId,
    ) -> Result<io::Result<Output>> {
        self.cache(login).await?;
        Ok(self.push_deletion(branch, head).await)
    }

    /// Hand `login` for the URL's host to the credential cache.
    async fn cache(&self, login: &GitLogin) -> Result<()> {
        let mut approve = self.git();
        approve.args(["credential", "approve"]);
        let request = format!(
            "protocol={}\nhost={}\nusername={}\npassword={}\n\n",
            self.scheme, self.host, login.username, login.password
        );
        let approved = (git::bounded(approve).input(request).checked().await)
            .context("hand the login to Git's credential cache")?;
        // Git reports a cache that failed to start without failing.
        ensure!(
            approved.stderr.is_empty(),
            "Git's credential cache did not take the login: {}",
            subprocess::diagnostic(&approved.stderr)
        );
        Ok(())
    }

    /// Push the deletion of `branch` while it points to `head`.
    async fn push_deletion(&self, branch: &BranchName, head: &CommitId) -> io::Result<Output> {
        let destination = git::local_ref(branch);
        let mut push = self.git();
        push.args(git::porcelain_push_args(
            &self.url,
            &[format!(":{destination}")],
            &[(&destination, head.as_str())],
        ));
        git::bounded(push).capture().await
    }

    /// Stop the credential cache, forgetting the login, and remove the
    /// repository.
    async fn close(self) -> Result<()> {
        let socket = self.socket();
        let socket = socket.to_string_lossy();
        let stopped = git::run_isolated(
            &self.repository(),
            &["credential-cache", "--socket", &socket, "exit"],
        )
        .await;
        off_runtime::run(move || Ok(self.directory.close()?)).await?;
        stopped.map(drop)
    }

    /// Git in the repository with only the credential cache to sign in
    /// with, for the URL's host alone, following no redirect elsewhere and
    /// asking no one for a login.
    fn git(&self) -> Command {
        let socket = self.socket();
        let socket = crate::shell::quote(&[socket.to_string_lossy()]);
        let helper = format!(
            "credential.{}://{}.helper=cache --timeout 900 --socket {socket}",
            self.scheme, self.host
        );
        let mut command = git::isolated_command(&self.repository());
        command
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_ASKPASS")
            .env_remove("SSH_ASKPASS")
            .args(["-c", "credential.helper=", "-c", &helper])
            .args(["-c", "http.followRedirects=false"]);
        command
    }

    /// The empty repository.
    fn repository(&self) -> PathBuf {
        self.directory.path().join("repository")
    }

    /// Where the credential cache listens.
    fn socket(&self) -> PathBuf {
        self.directory.path().join("credentials")
    }
}

/// The scheme and host of `url`.
fn scheme_and_host(url: &str) -> Result<(&str, &str)> {
    let (scheme, rest) = url
        .split_once("://")
        .context("the push URL names no scheme")?;
    Ok((scheme, rest.split('/').next().unwrap_or_default()))
}

/// A new temporary directory only this user reads.
async fn private_directory() -> Result<tempfile::TempDir> {
    off_runtime::run(|| {
        let private = std::fs::Permissions::from_mode(0o700);
        Ok(tempfile::Builder::new().permissions(private).tempdir()?)
    })
    .await
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
