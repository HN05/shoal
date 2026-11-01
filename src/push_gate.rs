//! Holding a tracked agent's `git push` for the user's approval where
//! `[permissions] push = "propose"`. Git settings in the agent's environment
//! rewrite the push URLs of the workspace repository's remotes to the
//! `shoal::` transport, so Git hands every push to them to Shoal's remote
//! helper, which asks the daemon before it pushes. Fetches keep their URLs.
pub mod helper;

use std::{
    ffi::{OsStr, OsString},
    os::unix::fs::PermissionsExt,
    path::Path,
};

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
pub struct Launch {
    settings: Vec<(String, String)>,
    helper: Option<tempfile::TempDir>,
}

impl Launch {
    /// The rewrites for the remotes of the repository at `worktree`, and
    /// the helper they reach.
    pub async fn prepare(paths: &Paths, worktree: &Path) -> Result<Self> {
        let settings = rewrites(&git::remote_urls(worktree).await?);
        if settings.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self {
            settings,
            helper: Some(write_helper(paths)?),
        })
    }

    /// The Git settings that send pushes through the helper.
    pub fn settings(&self) -> &[(String, String)] {
        &self.settings
    }

    /// Put the helper first on the child's PATH.
    pub fn apply(&self, command: &mut tokio::process::Command) {
        if let Some(helper) = &self.helper {
            env::prepend_path(command, helper.path().as_os_str());
        }
    }
}

/// Settings that send pushes to `urls` through the helper. A remote's URL
/// is rewritten for pushes only. An explicit push URL ignores push
/// rewrites, so one that nothing fetches from is rewritten for every use;
/// one that is also fetched from cannot be, since the helper only pushes.
fn rewrites(urls: &git::RemoteUrls) -> Vec<(String, String)> {
    let fetched = urls
        .fetch
        .iter()
        .map(|url| (format!("{PREFIX}{url}.pushInsteadOf"), url.clone()));
    let pushed = urls
        .push
        .iter()
        .filter(|url| !urls.fetch.contains(url))
        .map(|url| (format!("{PREFIX}{url}.insteadOf"), url.clone()));
    fetched.chain(pushed).collect()
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
    let words = crate::cli::internal::internal_command(
        paths,
        false,
        crate::cli::internal::Worker::PushGate,
    )?
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
/// push Shoal already authorized or the helper runs itself. The settings
/// are those `command` sets, else those this process inherited; the others
/// keep their order.
pub fn strip(command: &mut tokio::process::Command) {
    let set: Vec<(OsString, Option<OsString>)> = command
        .as_std()
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
        .collect();
    let read = |name: &str| match set.iter().find(|(set, _)| set == name) {
        Some((_, value)) => value.clone(),
        None => std::env::var_os(name),
    };
    let count = read("GIT_CONFIG_COUNT")
        .and_then(|count| count.into_string().ok())
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0);
    let entries: Vec<_> = (0..count)
        .map(|index| {
            (
                read(&format!("GIT_CONFIG_KEY_{index}")).unwrap_or_default(),
                read(&format!("GIT_CONFIG_VALUE_{index}")).unwrap_or_default(),
            )
        })
        .collect();
    let kept: Vec<_> = entries.iter().filter(|(key, _)| !is_rewrite(key)).collect();
    if kept.len() == entries.len() {
        return;
    }
    for index in 0..count {
        command
            .env_remove(format!("GIT_CONFIG_KEY_{index}"))
            .env_remove(format!("GIT_CONFIG_VALUE_{index}"));
    }
    for (index, (key, value)) in kept.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    command.env("GIT_CONFIG_COUNT", kept.len().to_string());
}

/// Whether the Git setting `key` is one of the rewrites; Git compares the
/// section and variable names without case.
fn is_rewrite(key: &OsStr) -> bool {
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
