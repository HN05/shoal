//! The push gate's Git: the URL rewrites that send an agent's pushes to
//! `git-remote-shoal`, the push gate's remote helper, and the Git commands
//! that helper runs without them, so they reach the real remote. See
//! `docs/design/permissions.md`.
use std::{ffi::OsStr, io, path::Path, process::Stdio};

use anyhow::{Context, Result};
use shoal_refs::CommitId;
use tokio::process::Command;

/// The transport the rewritten URLs name: Git runs `git-remote-shoal` for
/// a URL that starts with `shoal::`.
const PREFIX: &str = "url.shoal::";

/// The Git settings that send pushes to the remotes of the repository at
/// `worktree` through `git-remote-shoal`, as `(key, value)`.
pub async fn rewrites(worktree: &Path) -> Result<Vec<(String, String)>> {
    rewrites_of(&super::remote_urls(worktree).await?)
}

/// Settings that send pushes to `urls` through the helper. A remote's URL
/// is rewritten for pushes only. An explicit push URL ignores push
/// rewrites, so it is rewritten for every use, which Git applies to every
/// URL that starts with it. That fails for a fetch URL: the helper only
/// pushes, so the agent does not start.
fn rewrites_of(urls: &super::RemoteUrls) -> Result<Vec<(String, String)>> {
    for url in &urls.push {
        if let Some(fetch) = urls
            .fetch
            .iter()
            .find(|fetch| fetch.starts_with(url.as_str()))
        {
            anyhow::bail!(
                "the push URL {url} would also rewrite fetches from {fetch}, so Shoal cannot gate pushes to it; change or remove that pushurl, or give the agent a push permission of \"direct\""
            );
        }
    }
    let fetched = urls
        .fetch
        .iter()
        .map(|url| (format!("{PREFIX}{url}.pushInsteadOf"), url.clone()));
    let pushed = urls
        .push
        .iter()
        .map(|url| (format!("{PREFIX}{url}.insteadOf"), url.clone()));
    Ok(fetched.chain(pushed).collect())
}

/// Remove the rewrites from the Git settings `command` passes on, for a
/// push Shoal already authorized or the helper runs itself.
pub fn strip(command: &mut Command) {
    super::config_env::retain(command, |(key, _)| !is_rewrite(key));
}

/// Whether the Git setting `key` is one of the rewrites; Git compares the
/// section and variable names without case.
pub fn is_rewrite(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    let Some(rest) = key
        .get(..PREFIX.len())
        .filter(|start| start.eq_ignore_ascii_case(PREFIX))
        .map(|_| &key[PREFIX.len()..])
    else {
        return false;
    };
    let variable = rest.rsplit_once('.').map(|(_, variable)| variable);
    variable.is_some_and(|variable| {
        variable.eq_ignore_ascii_case("pushInsteadOf") || variable.eq_ignore_ascii_case("insteadOf")
    })
}

/// The remote's refs at `url`, as `git ls-remote` prints them.
pub async fn list_remote(url: &str) -> Result<String> {
    output(&["ls-remote", "--", url])
        .await
        .context("list the remote's refs")
}

/// The object `rev` names.
pub async fn resolve(rev: &str) -> Result<CommitId> {
    let object = output(&["rev-parse", "--verify", "--end-of-options", rev])
        .await
        .with_context(|| format!("resolve {rev}"))?;
    Ok(object.trim().into())
}

/// Every push URL of `remote`.
pub async fn push_urls(remote: &str) -> Result<Vec<String>> {
    let urls = output(&["remote", "get-url", "--push", "--all", remote]).await?;
    Ok(urls.lines().map(str::to_owned).collect())
}

/// Push `refspecs` to `url` without the hooks Git already ran, and return
/// what `git push --porcelain` printed. `leases` pairs each destination ref
/// with the object `--force-with-lease` expects there. Git's progress and
/// errors reach the user on this process's stderr, and a push has no time
/// limit, since its time grows with what it sends.
pub async fn porcelain_push(
    url: &str,
    refspecs: &[String],
    leases: &[(&str, &str)],
) -> io::Result<String> {
    let output = git(&super::porcelain_push_args(url, refspecs, leases))
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn output(args: &[&str]) -> Result<String> {
    super::bounded(git(args)).output().await
}

/// Git in the repository Git runs the helper for, which the variables it
/// exports name, without the rewrites.
fn git(args: &[impl AsRef<OsStr>]) -> Command {
    let mut command = Command::new(super::PROGRAM);
    command.args(args);
    strip(&mut command);
    command
}

#[cfg(test)]
mod tests;
