//! Deleting a remote branch as an account, only while it points to the
//! commit the caller checked: Git sends that commit with the deletion. The
//! push inherits no configuration or `.netrc`, which could send it elsewhere
//! or as someone else. See `docs/design/forge.md`.
use std::{
    io,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Output,
};

use anyhow::{Context, Result, bail, ensure};
use shoal_forge::transport::account::GitLogin;
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
    url: String,
    /// The scheme and host of `url`, which the login is for.
    scheme: String,
    host: String,
}

impl Scratch {
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

    async fn init(&self, head: &CommitId) -> Result<()> {
        let format = format!("--object-format={}", object_format(head));
        let repository = self.repository();
        let repository = repository.to_string_lossy();
        let mut init = self.unconfigured(self.directory.path());
        init.args(["init", "--quiet", "--bare", &format, &repository]);
        git::bounded(init).output().await?;
        Ok(())
    }

    async fn delete(
        &self,
        login: &GitLogin,
        branch: &BranchName,
        head: &CommitId,
    ) -> Result<io::Result<Output>> {
        self.cache(login).await?;
        Ok(self.push_deletion(branch, head).await)
    }

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
        let mut exit = self.unconfigured(&self.repository());
        exit.arg("credential-cache").args([
            "--socket".as_ref(),
            socket.as_os_str(),
            "exit".as_ref(),
        ]);
        let stopped = git::bounded(exit).output().await;
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
        let mut command = self.unconfigured(&self.repository());
        command
            .args(["-c", "credential.helper=", "-c", &helper])
            .args(["-c", "http.followRedirects=false"]);
        command
    }

    /// Git in `directory` without configuration beyond the repository's:
    /// none from the system, the user's files or the `GIT_` variables this
    /// process inherited, which could send it elsewhere, sign it in as
    /// someone else or log what it sends, and with the scratch directory as
    /// its home, which keeps out the user's `.netrc`, whose login libcurl
    /// would send first.
    fn unconfigured(&self, directory: &Path) -> Command {
        let mut command = git::isolated_command(directory);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.directory.path())
            .env_remove("SSH_ASKPASS");
        command
    }

    fn repository(&self) -> PathBuf {
        self.directory.path().join("repository")
    }

    fn socket(&self) -> PathBuf {
        self.directory.path().join("credentials")
    }
}

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

fn object_format(head: &CommitId) -> &'static str {
    match head.as_str().len() {
        64 => "sha256",
        _ => "sha1",
    }
}

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
