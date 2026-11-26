//! The last sessions a space keeps of its agents, which a restore continues.
use crate::paths::StateFiles;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::executions::TrackedAgent;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::{Ending, LAST_SESSION_SUFFIX, LastSession, Saved, SavedExt as _, carried, consume};
use crate::{
    fsutil::atomic::{Permissions, ReplaceOptions},
    paths::Paths,
    state::AgentState,
};

/// Ends the name of a last session written beside its place until it is
/// renamed there, which no reader of last sessions lists.
const STAGED_SUFFIX: &str = ".staged";

/// Operations on the client type [`LastSession`] that only the `shoal` command uses.
pub trait LastSessionExt: Sized {
    /// This session where it is not available, which starts a new one.
    fn moved(self) -> Self;

    /// How the session ended: as Shoal stopped it, or on its own, in the
    /// turn it kept.
    fn ending(&self) -> Ending<'_>;

    fn path(paths: &Paths, space_id: &SpaceId, execution: &ExecutionId) -> PathBuf;

    /// Save execution `id` of `agent`, which last reported `turn`, as one
    /// of the space's last sessions, continuing the session it continued.
    fn save(
        paths: &Paths,
        space_id: &SpaceId,
        agent: &TrackedAgent,
        id: &ExecutionId,
        turn: Option<AgentState>,
    ) -> Result<()>;

    fn write(&self, paths: &Paths, space_id: &SpaceId) -> Result<()>;

    /// Write this session beside its place, where it counts once committed.
    fn stage(&self, paths: &Paths, space_id: &SpaceId) -> Result<StagedLastSession>;

    /// Read the last session saved at `path`, `None` once it is consumed.
    fn load(path: &Path) -> Result<Option<Self>>;

    /// The space's last sessions; an unreadable one is left out.
    fn all(paths: &Paths, space_id: &SpaceId) -> Result<Vec<Self>>;

    fn forget_all(paths: &Paths, space_id: &SpaceId) -> Result<()>;
}

impl LastSessionExt for LastSession {
    fn moved(self) -> Self {
        Self {
            session: None,
            moved: true,
            origin: None,
            ..self
        }
    }

    fn ending(&self) -> Ending<'_> {
        Ending {
            stop: self.stop.as_ref(),
            turn: self.turn,
        }
    }

    fn path(paths: &Paths, space_id: &SpaceId, execution: &ExecutionId) -> PathBuf {
        paths
            .space_state(space_id)
            .join(format!("{execution}{LAST_SESSION_SUFFIX}"))
    }

    fn save(
        paths: &Paths,
        space_id: &SpaceId,
        agent: &TrackedAgent,
        id: &ExecutionId,
        turn: Option<AgentState>,
    ) -> Result<()> {
        let carried = carried::continued(paths, space_id, id);
        Self {
            execution: id.to_owned(),
            agent: agent.name.normalized(),
            session: carried.session,
            moved: carried.moved,
            copied: carried.copied,
            converted: carried.converted,
            handoff: carried.handoff,
            origin: None,
            stop: None,
            turn,
            model: agent.model.clone(),
            mode: agent.mode.clone(),
            narrowing: agent.narrowing,
        }
        .write(paths, space_id)
    }

    fn write(&self, paths: &Paths, space_id: &SpaceId) -> Result<()> {
        std::fs::create_dir_all(paths.space_state(space_id))?;
        crate::fsutil::atomic::replace_atomically(
            &Self::path(paths, space_id, &self.execution),
            &serde_json::to_vec(self)?,
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: true,
            },
        )?;
        Ok(())
    }

    fn stage(&self, paths: &Paths, space_id: &SpaceId) -> Result<StagedLastSession> {
        let path = Self::path(paths, space_id, &self.execution);
        let mut staged = path.clone().into_os_string();
        staged.push(STAGED_SUFFIX);
        let staged = StagedLastSession {
            staged: staged.into(),
            path,
            committed: false,
        };
        std::fs::create_dir_all(paths.space_state(space_id))?;
        crate::fsutil::atomic::replace_atomically(
            &staged.staged,
            &serde_json::to_vec(self)?,
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: true,
            },
        )?;
        Ok(staged)
    }

    fn load(path: &Path) -> Result<Option<Self>> {
        let execution = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(LAST_SESSION_SUFFIX))
            .with_context(|| format!("{} is not a last session", path.display()))?;
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(|last| {
                    Some(Self {
                        execution: ExecutionId::from(execution.to_owned()),
                        ..last
                    })
                })
                .with_context(|| format!("read {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn all(paths: &Paths, space_id: &SpaceId) -> Result<Vec<Self>> {
        Ok(Saved::load(paths, space_id)?
            .sessions
            .iter()
            .filter_map(|last| Self::load(&last.path).ok().flatten())
            .collect())
    }

    fn forget_all(paths: &Paths, space_id: &SpaceId) -> Result<()> {
        for last in Saved::load(paths, space_id)?.sessions {
            consume(&last.path)?;
        }
        Ok(())
    }
}

/// A last session written beside its place, so the steps before its commit
/// may fail and leave the space's last sessions as they were. Dropping it
/// uncommitted removes it.
#[must_use = "a staged last session counts only once committed"]
pub struct StagedLastSession {
    staged: PathBuf,
    path: PathBuf,
    committed: bool,
}

impl StagedLastSession {
    /// Rename the session into its place, replacing the one there.
    pub fn commit(mut self) -> Result<()> {
        std::fs::rename(&self.staged, &self.path)
            .with_context(|| format!("save {}", self.path.display()))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedLastSession {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.staged);
        }
    }
}
