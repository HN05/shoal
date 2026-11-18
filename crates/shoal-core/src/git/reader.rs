//! Reads of a repository's own state, such as its location, HEAD, refs,
//! remotes and config, in this process through gix instead of a Git
//! process. Where gix would not read a repository as Git does, such as one
//! that keeps its refs in a reftable, or where this process's environment
//! points Git at another repository, the read starts Git instead, so either
//! way it gives Git's answer. Each read opens the repository afresh and
//! keeps nothing, so it sees a moved or replaced worktree as Git would.
use std::path::Path;

use anyhow::Result;

use crate::fsutil::off_runtime;

/// The repository-format extensions with which gix reads a repository's
/// location, refs and config as Git does, lowercase as Git compares them.
const READABLE_EXTENSIONS: &[&str] = &[
    "noop",
    "noop-v1",
    "objectformat",
    "partialclone",
    "preciousobjects",
    "relativeworktrees",
    "worktreeconfig",
];

/// The reference storage gix reads.
const READABLE_REF_STORAGE: &str = "files";

/// The newest repository format gix reads.
const READABLE_FORMAT: i64 = 1;

/// Variables that change where Git finds a repository or how it reads one
/// in ways gix does not follow: the [`super::REPOSITORY_ENV`] that Shoal's
/// Git commands drop and gix would read, and the one discovery variable gix
/// leaves out.
const DIVERGING_ENV: &[&str] = &["GIT_DISCOVERY_ACROSS_FILESYSTEM"];

/// `in_process`'s answer for the repository Git finds from `path`, as `git
/// -C <path>` finds it, read on the blocking pool; `cli`'s, which runs Git,
/// where gix would not read that repository as Git does.
pub(crate) async fn read<T: Send + 'static>(
    path: &Path,
    in_process: impl FnOnce(&gix::Repository) -> Result<T> + Send + 'static,
    cli: impl AsyncFnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let at = path.to_owned();
    let read = off_runtime::run(move || Ok(open(&at).map(|repo| in_process(&repo)))).await?;
    match read {
        Some(answer) => answer,
        None => cli(path).await,
    }
}

/// The repository Git finds from `path`, opened as Git would open it: with
/// the user's and system configuration, refusing a repository another user
/// owns that `safe.directory` does not allow. `None` where Git might answer
/// differently, or fail where gix succeeds, which leaves the read to Git.
fn open(path: &Path) -> Option<gix::Repository> {
    if environment_diverges() {
        return None;
    }
    let options = gix::open::Options::default()
        .bail_if_untrusted(true)
        .strict_config(true);
    let trust = gix::sec::trust::Mapping {
        full: options.clone(),
        reduced: options,
    };
    let discovery = gix::discover::upwards::Options::default().apply_environment();
    let repo = gix::ThreadSafeRepository::discover_opts(path, discovery, trust).ok()?;
    let repo = repo.to_thread_local();
    readable(&repo).then_some(repo)
}

fn environment_diverges() -> bool {
    (super::REPOSITORY_ENV.iter().chain(DIVERGING_ENV)).any(|name| std::env::var_os(name).is_some())
}

/// Whether gix reads `repo` as Git does: its format and every extension it
/// enables are ones gix supports.
fn readable(repo: &gix::Repository) -> bool {
    let config = repo.config_snapshot();
    let format = config.integer("core.repositoryFormatVersion").unwrap_or(0);
    let local = |section: &gix::config::file::SectionRef<'_>| {
        section.meta().source == gix::config::Source::Local
    };
    let extensions = config.plumbing().sections_by_name("extensions");
    let readable_extensions = extensions
        .into_iter()
        .flatten()
        .filter(local)
        .all(|section| {
            section.value_names().all(|name| {
                let name = name.to_ascii_lowercase();
                READABLE_EXTENSIONS.contains(&name.as_str())
                    || (name == "refstorage"
                        && section.value("refstorage").is_some_and(|storage| {
                            storage.eq_ignore_ascii_case(READABLE_REF_STORAGE.as_bytes())
                        }))
            })
        });
    format <= READABLE_FORMAT && readable_extensions && !names_remotes_in_files(repo)
}

/// Whether `repo` names remotes in files of its `remotes/` or `branches/`
/// directory, an old way Git still reads and gix does not.
fn names_remotes_in_files(repo: &gix::Repository) -> bool {
    ["remotes", "branches"].iter().any(|directory| {
        std::fs::read_dir(repo.common_dir().join(directory))
            .is_ok_and(|mut entries| entries.next().is_some())
    })
}

#[cfg(test)]
mod tests;
