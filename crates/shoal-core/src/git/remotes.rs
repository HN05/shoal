//! A repository's remotes, their URLs and the upstreams of its branches,
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
/// unless the repository's own config has a section for the remote, as
/// `git remote get-url` requires, whatever other files define.
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
    let in_repository = |section: &&gix::config::file::SectionRef<'_>| {
        use gix::config::Source;
        matches!(section.meta().source, Source::Local | Source::Worktree)
    };
    if !sections.iter().any(|section| in_repository(&section)) {
        return None;
    }
    let urls = sections.iter().flat_map(|section| section.values("url"));
    let url = urls.map(|url| url.to_string()).next();
    Some(rewritten(file, &url.unwrap_or_else(|| name.to_owned())))
}

/// `url` with the longest `url.<base>.insteadOf` prefix it starts with
/// replaced by that base, as Git rewrites a fetch URL. Of equally long
/// prefixes, that of the base configured first wins, wherever the prefix
/// itself appears, since Git gathers each base's prefixes together.
fn rewritten(config: &gix::config::File, url: &str) -> String {
    let mut best: Option<(usize, String)> = None;
    for (base, prefixes) in rewrites(config) {
        for prefix in prefixes {
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

/// Each `url.<base>` of `config` with its `insteadOf` prefixes, in the order
/// Git meets each base's first such prefix: a section with none, such as
/// one with only `pushInsteadOf`, places nothing.
fn rewrites(config: &gix::config::File) -> Vec<(String, Vec<String>)> {
    let mut rewrites: Vec<(String, Vec<String>)> = Vec::new();
    for section in config.sections_by_name("url").into_iter().flatten() {
        let Some(base) = section.header().subsection_name() else {
            continue;
        };
        let base = base.to_string();
        let prefixes: Vec<String> = (section.values("insteadOf").into_iter())
            .map(|prefix| prefix.to_string())
            .collect();
        if prefixes.is_empty() {
            continue;
        }
        match rewrites.iter_mut().find(|(known, _)| *known == base) {
            Some((_, known)) => known.extend(prefixes),
            None => rewrites.push((base, prefixes)),
        }
    }
    rewrites
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

/// What a local branch's configuration says it tracks.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Upstream {
    /// `branch.<name>.remote`: a remote's name, or `.` for a local branch.
    pub(crate) remote: String,
    /// `branch.<name>.merge`: the full name of the branch on that remote.
    pub(crate) merge: String,
    /// The ref that tracks it here, as the remote's fetch refspecs map it,
    /// or the local branch itself; empty when none does.
    pub(crate) tracking: String,
}

/// The upstream of the local branch `reference`, a full name, as `git
/// for-each-ref`'s `upstream` fields show it, each empty where it is not
/// configured; `None` when the branch does not exist.
pub(crate) async fn upstream(repo: &Path, reference: &str) -> Result<Option<Upstream>> {
    let owned = reference.to_owned();
    let in_process = move |repo: &gix::Repository| {
        if !super::refs::exists_in_process(repo, &owned)? {
            return Ok(None);
        }
        let branch = super::strip_local(&owned).unwrap_or(&owned);
        Ok(Some(upstream_in_process(&repo.config_snapshot(), branch)))
    };
    let git = async |repo: &Path| upstream_from_git(repo, reference).await;
    reader::read(repo, in_process, git).await
}

fn upstream_in_process(config: &gix::config::Snapshot<'_>, branch: &str) -> Upstream {
    let file = config.plumbing();
    let remote = file.string(format!("branch.{branch}.remote").as_str());
    let merge = file.strings(format!("branch.{branch}.merge").as_str());
    let remote = remote.map(|remote| remote.to_string()).unwrap_or_default();
    let merge = merge.and_then(|merge| merge.first().map(|merge| merge.to_string()));
    let merge = merge.unwrap_or_default();
    let tracking = match remote.as_str() {
        _ if merge.is_empty() => String::new(),
        "" => String::new(),
        "." => merge.clone(),
        remote => {
            let refspecs = file.strings(format!("remote.{remote}.fetch").as_str());
            let refspecs = refspecs.unwrap_or_default();
            let mapped = refspecs
                .iter()
                .find_map(|refspec| mapped(&refspec.to_string(), &merge));
            mapped.unwrap_or_default()
        }
    };
    Upstream {
        remote,
        merge,
        tracking,
    }
}

/// Where the fetch refspec `refspec` stores the remote ref `name`, if it
/// fetches it: its destination, with a `*` standing for what the source's
/// `*` matched.
fn mapped(refspec: &str, name: &str) -> Option<String> {
    let (source, destination) = refspec.trim_start_matches('+').split_once(':')?;
    if refspec.starts_with('^') || destination.is_empty() {
        return None;
    }
    match source.split_once('*') {
        None => (source == name).then(|| destination.to_owned()),
        Some((prefix, suffix)) => {
            let matched = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some(destination.replacen('*', matched, 1))
        }
    }
}

/// The [`Upstream`] Git lists for `reference` in `repo`.
async fn upstream_from_git(repo: &Path, reference: &str) -> Result<Option<Upstream>> {
    let format =
        "--format=%(refname)%00%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream)";
    let listed = run(repo, &["for-each-ref", format, reference]).await?;
    // The pattern also matches branches nested under the name.
    Ok(listed.lines().find_map(|line| {
        let mut fields = line.split('\0');
        (fields.next()? == reference).then(|| Upstream {
            remote: fields.next().unwrap_or_default().to_owned(),
            merge: fields.next().unwrap_or_default().to_owned(),
            tracking: fields.next().unwrap_or_default().to_owned(),
        })
    }))
}

#[cfg(test)]
mod tests;
