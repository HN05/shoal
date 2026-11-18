//! A repository's remotes and their URLs,
//! read from its configuration in this process where the
//! [`reader`](super::reader) reads the repository, else by Git.
use std::path::Path;

use anyhow::{Context, Result};
use shoal_refs::BranchName;

use super::{reader, run};

/// The names of the repository's remotes, sorted as `git remote` lists
/// them, for an operation that passes them to each lookup that picks among
/// them.
pub async fn remote_names(repo: &Path) -> Result<Vec<String>> {
    let in_process = |repo: &gix::Repository| {
        let names = repo.remote_names();
        Ok(names.iter().map(|name| name.to_string()).collect())
    };
    let git = async |repo: &Path| {
        let listing = run(repo, &["remote"]).await?;
        Ok(listing.lines().map(str::to_owned).collect())
    };
    reader::read(repo, in_process, git).await
}

/// The URLs the repository's remotes name, as configured, before any
/// rewriting.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RemoteUrls {
    /// Each `remote.<name>.url`, which pushes use unless a push URL is set.
    pub(crate) fetch: Vec<String>,
    /// Each `remote.<name>.pushurl`.
    pub(crate) push: Vec<String>,
}

impl RemoteUrls {
    /// Add the value of `remote.<name>.<variable>` once, unless empty.
    fn add(&mut self, variable: &str, value: &str) {
        let list = match variable.to_ascii_lowercase().as_str() {
            "url" => &mut self.fetch,
            "pushurl" => &mut self.push,
            _ => return,
        };
        if !value.is_empty() && !list.iter().any(|url| url == value) {
            list.push(value.to_owned());
        }
    }
}

pub(crate) async fn remote_urls(repo: &Path) -> Result<RemoteUrls> {
    let in_process = |repo: &gix::Repository| {
        let config = repo.config_snapshot();
        let mut urls = RemoteUrls::default();
        let sections = config.plumbing().sections_by_name("remote");
        for section in sections.into_iter().flatten() {
            for variable in ["url", "pushurl"] {
                for value in section.values(variable) {
                    urls.add(variable, &value.to_string());
                }
            }
        }
        Ok(urls)
    };
    reader::read(repo, in_process, remote_urls_from_git).await
}

/// The [`RemoteUrls`] Git lists for `repo`.
async fn remote_urls_from_git(repo: &Path) -> Result<RemoteUrls> {
    let listed = run(repo, &["config", "--null", "--list"]).await?;
    let mut urls = RemoteUrls::default();
    for entry in listed.split('\0') {
        let Some((key, value)) = entry.split_once('\n') else {
            continue;
        };
        if let Some((_, variable)) = key
            .strip_prefix("remote.")
            .and_then(|rest| rest.rsplit_once('.'))
        {
            urls.add(variable, value);
        }
    }
    Ok(urls)
}

pub async fn remote_url(repo: &Path, remote: &str) -> Result<String> {
    configured_remote_url(repo, remote)
        .await?
        .with_context(|| format!("No such remote '{remote}'"))
}

/// Git's exit status for a remote the repository does not configure.
const NO_SUCH_REMOTE: i32 = 2;

/// The URL `git remote get-url` gives for the remote `remote`, with
/// `url.<base>.insteadOf` applied, or `None` when the repository has no
/// such remote.
pub async fn configured_remote_url(repo: &Path, remote: &str) -> Result<Option<String>> {
    let name = remote.to_owned();
    let in_process = move |repo: &gix::Repository| Ok(fetch_url(repo, &name));
    let git = async |repo: &Path| {
        let mut command = super::command(repo);
        command.args(["remote", "get-url", remote]);
        let url = super::output_unless(command, NO_SUCH_REMOTE).await?;
        Ok(url.map(|url| url.trim().to_owned()))
    };
    reader::read(repo, in_process, git).await
}

/// The fetch URL of the remote `name` in `repo`'s configuration: its first
/// `url`, or its name where it has none, as Git takes it, rewritten; `None`
/// when no section configures the remote.
fn fetch_url(repo: &gix::Repository, name: &str) -> Option<String> {
    let config = repo.config_snapshot();
    let file = config.plumbing();
    let sections: Vec<_> = (file.sections_by_name("remote").into_iter().flatten())
        .filter(|section| {
            section
                .header()
                .subsection_name()
                .is_some_and(|sub| sub == name)
        })
        .collect();
    if sections.is_empty() {
        return None;
    }
    let urls = sections.iter().flat_map(|section| section.values("url"));
    let url = urls.map(|url| url.to_string()).next();
    Some(rewritten(file, &url.unwrap_or_else(|| name.to_owned())))
}

/// `url` with the longest `url.<base>.insteadOf` prefix it starts with
/// replaced by that base, as Git rewrites a fetch URL; the first of equally
/// long prefixes wins.
fn rewritten(config: &gix::config::File, url: &str) -> String {
    let mut best: Option<(usize, String)> = None;
    for section in config.sections_by_name("url").into_iter().flatten() {
        let Some(base) = section.header().subsection_name() else {
            continue;
        };
        for prefix in section.values("insteadOf") {
            let prefix = prefix.to_string();
            let longer = best
                .as_ref()
                .is_none_or(|(length, _)| prefix.len() > *length);
            if url.starts_with(&prefix) && longer {
                best = Some((prefix.len(), format!("{base}{}", &url[prefix.len()..])));
            }
        }
    }
    best.map_or_else(|| url.to_owned(), |(_, url)| url)
}

/// The remote a push of `branch` goes to: `branch.<name>.pushRemote`, else
/// `remote.pushDefault`, else `origin`.
pub async fn push_remote(repo: &Path, branch: &BranchName) -> Result<String> {
    for key in [
        format!("branch.{branch}.pushRemote"),
        "remote.pushDefault".into(),
    ] {
        if let Some(value) = super::config_value(repo, &key).await? {
            return Ok(value);
        }
    }
    Ok("origin".into())
}

#[cfg(test)]
mod tests;
