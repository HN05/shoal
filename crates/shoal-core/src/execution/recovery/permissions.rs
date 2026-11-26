//! Changing the permissions that saved agents and last sessions restore with.
use crate::config::permissions::PermissionChange;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;

use super::{
    LastSession, LastSessionExt as _, Record, RecordExt as _, RecoveryClaim, Saved, SavedExt as _,
    SavedSession, claim, rewrite,
};
use crate::paths::Paths;

/// Make `change` to the permissions given to each agent and last session
/// the `saved` lists keep, which its restore runs under, holding the claim a
/// restore takes on each of them, so one a restore holds is refused before
/// any changes.
pub(crate) fn change_permissions(saved: &[Saved], change: &PermissionChange) -> Result<()> {
    ClaimedChange::claim(saved)?.make(change)
}

/// The agents and last sessions a permission change rewrites, each claimed
/// as a restore claims it until the change is made.
pub struct ClaimedChange<'a> {
    saved: &'a [Saved],
    _claims: Vec<RecoveryClaim>,
}

impl<'a> ClaimedChange<'a> {
    /// Claim each agent and last session `saved` lists, refusing one a
    /// restore holds.
    pub fn claim(saved: &'a [Saved]) -> Result<Self> {
        let paths = saved
            .iter()
            .flat_map(|saved| saved.agents.iter().chain(&saved.sessions));
        let claims = paths
            .map(|saved| claim(&saved.path))
            .collect::<Result<_>>()?;
        Ok(Self {
            saved,
            _claims: claims,
        })
    }

    pub fn make(self, change: &PermissionChange) -> Result<()> {
        let agents = self.saved.iter().flat_map(|saved| &saved.agents);
        let sessions = self.saved.iter().flat_map(|saved| &saved.sessions);
        for SavedSession { path, .. } in agents {
            if let Some(record) = Record::load(path)? {
                let narrowing = change.applied_to(record.narrowing);
                rewrite(
                    path,
                    &Record {
                        narrowing,
                        ..record
                    },
                )?;
            }
        }
        for SavedSession { path, .. } in sessions {
            if let Some(last) = LastSession::load(path)? {
                let narrowing = change.applied_to(last.narrowing);
                rewrite(path, &LastSession { narrowing, ..last })?;
            }
        }
        Ok(())
    }
}

/// Make `change` to the permissions given to the saved agent or last
/// session of execution `id` of the space `space_id`.
pub fn change_permission(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    change: &PermissionChange,
) -> Result<()> {
    let mut saved = Saved::load(paths, space_id)?;
    saved.retain_execution(id);
    change_permissions(&[saved], change)
}
