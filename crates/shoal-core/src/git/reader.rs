//! Reads of a repository's own state in process through gix, which start Git
//! instead where gix would read the repository otherwise. See
//! `docs/design/git.md`.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::LazyLock,
};

use anyhow::Result;

use crate::fsutil::off_runtime;

/// The repository-format extensions with which gix reads a repository's
/// location, refs and config as Git does, lowercase as Git compares them. A
/// partial clone is not among them: Git fetches an object it lacks, gix
/// does not.
const READABLE_EXTENSIONS: &[&str] = &[
    "noop",
    "noop-v1",
    "objectformat",
    "preciousobjects",
    "relativeworktrees",
    "worktreeconfig",
];

const READABLE_REF_STORAGE: &str = "files";

/// The newest repository format gix reads.
const READABLE_FORMAT: i64 = 1;

/// Variables that change where Git finds a repository or how it reads one
/// in ways gix does not follow: the [`super::REPOSITORY_ENV`] that Shoal's
/// Git commands drop and gix would read, those Git reads and gix leaves
/// out, such as the `-c` settings Git passes on to hooks and aliases or the
/// one file `git config` reads under `GIT_CONFIG`, and
/// the ref namespace, which gix applies to every ref it reads and Git only
/// to what it serves a fetch or push.
const DIVERGING_ENV: &[&str] = &[
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_NAMESPACE",
];

/// `in_process`'s answer for the repository Git finds from `path`, as `git
/// -C <path>` finds it, read on the blocking pool; `cli`'s, which runs Git,
/// where gix would not read that repository as Git does.
pub(crate) async fn read<T: Send + 'static>(
    path: &Path,
    in_process: impl FnOnce(&gix::Repository) -> Result<T> + Send + 'static,
    cli: impl AsyncFnOnce(&Path) -> Result<T>,
) -> Result<T> {
    read_unless_deferred(path, |repo| in_process(repo).map(Some), cli).await
}

/// As [`read`], where `in_process` may also leave the read to `cli` by
/// answering `None`, for an input whose meaning depends on the repository.
pub(crate) async fn read_unless_deferred<T: Send + 'static>(
    path: &Path,
    in_process: impl FnOnce(&gix::Repository) -> Result<Option<T>> + Send + 'static,
    cli: impl AsyncFnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let at = path.to_owned();
    let read = off_runtime::run(move || open(&at).map(|repo| in_process(&repo)).transpose());
    match read.await? {
        Some(Some(answer)) => Ok(answer),
        _ => cli(path).await,
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
    let mut options = gix::open::Options::default()
        .permissions(permissions())
        .bail_if_untrusted(true)
        .strict_config(true);
    if system_config_applies()
        && let Some(system) = system_config()
    {
        options = options.system_config_path(system);
    }
    let trust = gix::sec::trust::Mapping {
        full: options.clone(),
        reduced: options,
    };
    let discovery = gix::discover::upwards::Options {
        // Git passes over a ceiling directory that holds none of `path`.
        match_ceiling_dir_or_error: false,
        ..Default::default()
    }
    .apply_environment();
    let ceilings = discovery.ceiling_dirs.clone();
    let repo = gix::ThreadSafeRepository::discover_opts(path, discovery, trust).ok()?;
    let repo = repo.to_thread_local();
    (readable(&repo) && !found_in_ceiling(&repo, path, &ceilings)).then_some(repo)
}

/// Whether gix found `repo` in one of the `ceilings` above `path`, which
/// gix searches and Git, never entering a ceiling directory, does not.
fn found_in_ceiling(repo: &gix::Repository, path: &Path, ceilings: &[PathBuf]) -> bool {
    if ceilings.is_empty() {
        return false;
    }
    let found = repo.workdir().unwrap_or(repo.git_dir());
    match (found.canonicalize(), path.canonicalize()) {
        (Ok(found), Ok(path)) => found != path && ceilings.contains(&found),
        _ => true,
    }
}

/// What gix may read: by default all Git reads, and also the configuration
/// of the Git installation Shoal's Git commands run, such as Homebrew Git's
/// system file outside `/etc`, which gix asks that Git for once per process.
fn permissions() -> gix::open::Permissions {
    let mut permissions = gix::open::Permissions::default();
    permissions.config.git_binary = true;
    permissions
}

/// Whether gix reads the system config file [`system_config`] names: it
/// applies `GIT_CONFIG_SYSTEM` itself, unless given a path, and reads no
/// system file under a true `GIT_CONFIG_NOSYSTEM`.
fn system_config_applies() -> bool {
    let no_system = std::env::var_os("GIT_CONFIG_NOSYSTEM")
        .and_then(|value| gix::config::Boolean::try_from(value).ok())
        .is_some_and(|value| value.0);
    std::env::var_os("GIT_CONFIG_SYSTEM").is_none() && !no_system
}

/// The variable through which a process hands [`system_config`]'s answer
/// down to the processes it starts, as `<git>\n<file>`: the Git executable
/// on `PATH` that named it, then the file.
const HANDED_DOWN_SYSTEM_CONFIG: &str = "SHOAL_GIT_SYSTEM_CONFIG";

/// The system config file of the Git on `PATH`, the one Shoal's Git
/// commands run, as `git var GIT_CONFIG_SYSTEM` names it: `/etc/gitconfig`
/// for most, a file under its prefix for one such as Homebrew's, which
/// reads no other. Once per process, taken from the parent where it handed
/// down that Git's answer, else asked; `None` where that Git cannot say,
/// which leaves gix its own default.
fn system_config() -> Option<&'static Path> {
    static SYSTEM: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        handed_down_system_config(git_on_path().as_deref()).or_else(ask_system_config)
    });
    SYSTEM.as_deref()
}

/// The Git executable `PATH` finds, which Shoal's Git commands run,
/// canonical: a relative `PATH` entry finds another Git in another
/// directory.
fn git_on_path() -> Option<PathBuf> {
    let search_path = std::env::var_os("PATH")?;
    let found = super::find(&search_path)?;
    std::fs::canonicalize(found).ok()
}

fn handed_down_system_config(git: Option<&Path>) -> Option<PathBuf> {
    let value = std::env::var(HANDED_DOWN_SYSTEM_CONFIG).ok()?;
    let (named_by, file) = value.split_once('\n')?;
    (git == Some(Path::new(named_by)) && !file.is_empty()).then(|| PathBuf::from(file))
}

fn ask_system_config() -> Option<PathBuf> {
    let mut command = std::process::Command::new(super::PROGRAM);
    command
        .args(["--no-optional-locks", "var", "GIT_CONFIG_SYSTEM"])
        .env_remove("GIT_CONFIG_SYSTEM")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for name in super::REPOSITORY_ENV {
        command.env_remove(name);
    }
    let output = command
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim_end_matches('\n');
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Hand [`system_config`]'s answer down to the processes this one starts,
/// which take it instead of asking while `PATH` finds the same Git. Nothing
/// is handed down where gix reads no such file, as where
/// `GIT_CONFIG_SYSTEM` names the file instead.
///
/// # Safety
/// Call before any other thread exists.
pub unsafe fn hand_down_system_config() {
    if !system_config_applies() {
        return;
    }
    let (Some(git), Some(file)) = (git_on_path(), system_config()) else {
        return;
    };
    let (Some(git), Some(file)) = (git.to_str(), file.to_str()) else {
        return;
    };
    if !git.contains('\n') {
        let value = format!("{git}\n{file}");
        // SAFETY: the caller guarantees no other thread reads the environment.
        unsafe { std::env::set_var(HANDED_DOWN_SYSTEM_CONFIG, value) };
    }
}

/// The variable through which this process hands its system config file
/// down, with its value, for a process started in another environment,
/// such as a caller's; `None` where it hands nothing down.
pub fn system_config_variable() -> Option<(&'static str, std::ffi::OsString)> {
    let value = std::env::var_os(HANDED_DOWN_SYSTEM_CONFIG)?;
    Some((HANDED_DOWN_SYSTEM_CONFIG, value))
}

/// Variables that name config files, which Git, run as `git -C <path>`,
/// resolves from `path` where relative, and gix from this process's working
/// directory.
const CONFIG_FILE_ENV: &[&str] = &["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM"];

fn environment_diverges() -> bool {
    let set = (super::REPOSITORY_ENV.iter().chain(DIVERGING_ENV))
        .any(|name| std::env::var_os(name).is_some());
    let relative = CONFIG_FILE_ENV
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|path| Path::new(&path).is_relative()));
    set || relative
}

fn readable(repo: &gix::Repository) -> bool {
    let config = repo.config_snapshot();
    format_readable(&config)
        && !has_promisor_remote(&config)
        && includes_alike(repo, &config)
        && !names_remotes_in_files(repo)
}

fn format_readable(config: &gix::config::Snapshot<'_>) -> bool {
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
            section
                .value_names()
                .all(|name| extension_readable(&section, &name))
        });
    format <= READABLE_FORMAT && readable_extensions
}

fn extension_readable(section: &gix::config::file::SectionRef<'_>, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    READABLE_EXTENSIONS.contains(&name.as_str())
        || (name == "refstorage"
            && section.value("refstorage").is_some_and(|storage| {
                storage.eq_ignore_ascii_case(READABLE_REF_STORAGE.as_bytes())
            }))
}

/// Whether gix includes for `repo` the files `config` includes, in the
/// order Git's includes give them precedence.
fn includes_alike(repo: &gix::Repository, config: &gix::config::Snapshot<'_>) -> bool {
    one_file_per_include(config) && conditions_alike(repo, config)
}

/// Whether every include section of `config` names at most one file: gix
/// gives several files of one section the opposite precedence to Git's.
fn one_file_per_include(config: &gix::config::Snapshot<'_>) -> bool {
    let file = config.plumbing();
    let sections = ["include", "includeIf"]
        .into_iter()
        .flat_map(|name| file.sections_by_name(name));
    sections
        .flatten()
        .all(|section| section.values("path").len() <= 1)
}

/// Whether every conditional include of `config` includes for `repo` what
/// Git's would. Only a condition on the Git directory does: gix evaluates
/// one on the branch or on other configuration in its own way, such as
/// against HEAD's first symbolic target or before later files load, and
/// in a linked worktree matches one on the Git directory against the
/// common directory, where Git takes the worktree's own.
fn conditions_alike(repo: &gix::Repository, config: &gix::config::Snapshot<'_>) -> bool {
    let linked = is_linked_worktree(repo);
    let common = std::fs::canonicalize(repo.common_dir()).ok();
    let includes = config.plumbing().sections_by_name("includeIf");
    let mut conditions = (includes.into_iter().flatten()).filter_map(|section| {
        section
            .header()
            .subsection_name()
            .map(|name| name.to_string())
    });
    conditions.all(|condition| {
        let lowercase = condition.to_ascii_lowercase();
        let on_git_directory =
            lowercase.starts_with("gitdir:") || lowercase.starts_with("gitdir/i:");
        let alike = |common: &Path| matches_alike(&condition, common);
        on_git_directory && (!linked || common.as_deref().is_some_and(alike))
    })
}

fn is_linked_worktree(repo: &gix::Repository) -> bool {
    repo.git_dir() != repo.common_dir()
}

/// Whether the condition `gitdir:<pattern>` matches a linked worktree's
/// own Git directory, which lies inside `common`, exactly where it matches
/// `common`: a directory, ending in `/`, absolute or under `~/`, without
/// glob characters, that does not lie inside `common`. Any other pattern
/// might tell them apart.
fn matches_alike(condition: &str, common: &Path) -> bool {
    let Some(prefix) = condition
        .strip_prefix("gitdir:")
        .and_then(|p| p.strip_suffix('/'))
    else {
        return false;
    };
    if prefix.contains(['*', '?', '[', '\\']) {
        return false;
    }
    let prefix = match prefix.strip_prefix("~/") {
        Some(rest) => match gix::path::env::home_dir() {
            Some(home) => home.join(rest),
            None => return false,
        },
        None if prefix.starts_with('/') => PathBuf::from(prefix),
        None => return false,
    };
    let prefix = std::fs::canonicalize(&prefix).unwrap_or(prefix);
    !prefix.starts_with(common)
}

/// Whether a remote of `config` promises objects the repository lacks,
/// which makes it a partial clone without the extension: Git fetches such
/// an object when a read needs it, gix does not.
fn has_promisor_remote(config: &gix::config::Snapshot<'_>) -> bool {
    let file = config.plumbing();
    let remotes = file.sections_by_name("remote").into_iter().flatten();
    let mut names =
        remotes.filter_map(|section| section.header().subsection_name().map(ToOwned::to_owned));
    names.any(|name| {
        let key = format!("remote.{name}.promisor");
        // A value that is no boolean leaves the read to Git, which refuses it.
        file.boolean(key.as_str())
            .map_or(true, |promisor| promisor == Some(true))
    })
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
