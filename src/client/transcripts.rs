//! Where agent tools keep their sessions' files on this machine, such as a
//! session's transcript. Shoal finds them by name and never reads them: an
//! app that shows a conversation reads its tool's own format.
mod claude;
mod codex;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

use anyhow::Result;

use crate::client::ai::IntegrationKind;

/// What an agent runs with, which says where its tool keeps its files: this
/// process's environment with `configured`, the workspace's `[env]` for a
/// tracked agent.
pub struct AgentEnvironment<'a> {
    pub home: &'a Path,
    pub configured: &'a BTreeMap<String, OsString>,
}

impl<'a> AgentEnvironment<'a> {
    /// This process's environment alone, which agents Shoal does not track,
    /// such as chats and desktop apps, run with.
    pub fn inherited(home: &'a Path) -> Self {
        static NONE: BTreeMap<String, OsString> = BTreeMap::new();
        Self {
            home,
            configured: &NONE,
        }
    }
}

/// Every environment a repository's agents may have run with: this
/// process's under a home directory, and each `[env]` the daemon kept that
/// its tracked agents ran with.
pub struct Environments {
    home: PathBuf,
    configured: Vec<BTreeMap<String, OsString>>,
}

impl Environments {
    /// The environments under `home` with `agent_environments`, the
    /// `[env]` values the daemon kept.
    pub fn new(home: &Path, agent_environments: &[BTreeMap<String, String>]) -> Self {
        let configured = agent_environments
            .iter()
            .map(|environment| {
                environment
                    .iter()
                    .map(|(name, value)| (name.clone(), value.into()))
                    .collect()
            })
            .collect();
        Self {
            home: home.to_owned(),
            configured,
        }
    }

    /// Each environment, this process's first.
    pub fn each(&self) -> Vec<AgentEnvironment<'_>> {
        std::iter::once(AgentEnvironment::inherited(&self.home))
            .chain(self.configured.iter().map(|configured| AgentEnvironment {
                home: &self.home,
                configured,
            }))
            .collect()
    }
}

/// How an agent tool keeps its sessions' files.
pub trait Store: Sync {
    /// The directory the tool keeps its sessions' files in, running in
    /// `environment`.
    fn session_dir(&self, environment: &AgentEnvironment) -> Result<PathBuf>;

    /// The files of session `id` in `directory`, which
    /// [`Self::session_dir`] gave, relative to it.
    fn session_files(&self, directory: &Path, id: &str) -> std::io::Result<SessionFiles>;
}

/// A session's files in its tool's session directory, relative to it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SessionFiles {
    /// The conversation itself, when the session has one.
    pub transcript: Option<PathBuf>,
    /// Every file of the session, the transcript included.
    pub files: Vec<PathBuf>,
}

/// How the tool of an agent with integration `kind` keeps its sessions'
/// files; `None` for one whose files Shoal cannot find.
pub fn store(kind: IntegrationKind) -> Option<&'static dyn Store> {
    match kind {
        IntegrationKind::Claude => Some(&claude::Claude),
        IntegrationKind::Codex => Some(&codex::Codex),
        IntegrationKind::None => None,
    }
}

/// Where a session's files are: its tool's session directory, and its
/// files there.
#[derive(Debug, PartialEq, Eq)]
pub struct Found {
    pub directory: PathBuf,
    pub files: SessionFiles,
}

/// The files of session `id`, kept as `store` keeps them, in the first of
/// `environments` whose session directory has any; `None` when none has.
pub fn find(
    store: &dyn Store,
    environments: &[AgentEnvironment],
    id: &str,
) -> Result<Option<Found>> {
    if !is_file_name(id) {
        return Ok(None);
    }
    for environment in environments {
        let directory = store.session_dir(environment)?;
        let files = store.session_files(&directory, id)?;
        if !files.files.is_empty() {
            return Ok(Some(Found { directory, files }));
        }
    }
    Ok(None)
}

/// Whether `id` names a single file, so a session ID cannot reach outside
/// the directory it is looked up in.
fn is_file_name(id: &str) -> bool {
    let mut components = Path::new(id).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// Every file under `directory`, or with `files` false every directory in
/// it, relative to `base`, following no links; none when it is missing.
fn files_under(base: &Path, directory: &Path, files: bool) -> std::io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let wanted = match files {
                true => kind.is_file(),
                false => kind.is_dir(),
            };
            if wanted && let Ok(relative) = entry.path().strip_prefix(base) {
                found.push(relative.to_owned());
            }
            if files && kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests;
