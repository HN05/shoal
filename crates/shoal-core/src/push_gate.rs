//! Holding a tracked agent's `git push` for the user's approval where
//! its `push` permission is `"propose"`, or refusing it where it is `"deny"`.
//! Git settings in the agent's environment rewrite the push URLs of the
//! workspace repository's remotes to the `shoal::` transport, so Git hands
//! every push to them to Shoal's remote helper, which asks the daemon before
//! it pushes. Fetches keep their URLs.
use crate::paths::PathsExt;
use std::{ffi::OsStr, os::unix::fs::PermissionsExt, path::Path};

use anyhow::{Context, Result};

use crate::{env, git, paths::Paths};

/// The transport the rewritten URLs name: Git runs `git-remote-shoal` for
/// a URL that starts with `shoal::`.
const PREFIX: &str = "url.shoal::";
/// The remote helper Git looks for on the PATH.
const HELPER: &str = "git-remote-shoal";

/// The push URL rewrites an agent launch adds to the agent's Git settings,
/// and the directory with the remote helper they reach.
#[derive(Debug, Default)]
pub(crate) struct Launch {
    settings: Vec<(String, String)>,
    helper: Option<tempfile::TempDir>,
}

impl Launch {
    /// The rewrites for the remotes of the repository at `worktree`, and
    /// the helper they reach.
    pub(crate) async fn prepare(paths: &Paths, worktree: &Path) -> Result<Self> {
        let settings = rewrites(&git::remote_urls(worktree).await?)?;
        if settings.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self {
            settings,
            helper: Some(write_helper(paths)?),
        })
    }

    /// The Git settings that send pushes through the helper.
    pub(crate) fn settings(&self) -> &[(String, String)] {
        &self.settings
    }

    /// Put the helper first on the child's PATH.
    pub(crate) fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(helper) = &self.helper {
            env::prepend_path(command, helper.path().as_os_str());
        }
    }
}

/// Settings that send pushes to `urls` through the helper. A remote's URL
/// is rewritten for pushes only. An explicit push URL ignores push
/// rewrites, so it is rewritten for every use, which Git applies to every
/// URL that starts with it. That fails for a fetch URL: the helper only
/// pushes, so the agent does not start.
fn rewrites(urls: &git::RemoteUrls) -> Result<Vec<(String, String)>> {
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

/// The helper: a launcher that hands Git's arguments to this binary.
fn write_helper(paths: &Paths) -> Result<tempfile::TempDir> {
    let directory = paths.push_gate_directory()?;
    std::env::join_paths([directory.path()]).with_context(|| {
        format!(
            "the push helper cannot go on PATH from {}",
            directory.path().display()
        )
    })?;
    let words = crate::workers::internal_command(paths, false, crate::workers::Worker::PushGate)?
        .into_iter()
        .map(|word| {
            word.into_string()
                .map_err(|word| anyhow::anyhow!("{} is not UTF-8", word.to_string_lossy()))
        })
        .collect::<Result<Vec<_>>>()?;
    let path = directory.path().join(HELPER);
    let invocation = crate::shell::quote(&words);
    std::fs::write(&path, format!("#!/bin/sh\nexec {invocation} \"$@\"\n"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(directory)
}

/// Remove the rewrites from the Git settings `command` passes on, for a
/// push Shoal already authorized or the helper runs itself.
pub fn strip(command: &mut tokio::process::Command) {
    crate::agent_git::retain(command, |(key, _)| !is_rewrite(key));
}

/// Whether the Git setting `key` is one of the rewrites; Git compares the
/// section and variable names without case.
pub(crate) fn is_rewrite(key: &OsStr) -> bool {
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

#[cfg(test)]
mod tests;
