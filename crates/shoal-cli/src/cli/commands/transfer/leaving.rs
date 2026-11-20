//! Leaving a moved workspace's worktree: a shell inside the worktree of a
//! workspace the move ends goes elsewhere once it ended.
use anyhow::Result;
use std::path::PathBuf;

use crate::cli::{context::Context, shell};
use crate::transfer::MoveReport;
use shoal_ids::space_ids::SpaceId;
use shoal_workers::moves::{Planned, checks::Ending};

/// The workspace the move ends whose worktree holds the current
/// directory, with where the shell goes once it ended.
pub(super) struct Leaving(Option<(SpaceId, PathBuf)>);

impl Leaving {
    /// The workspace among those `planned` ends whose worktree holds the
    /// current directory.
    pub(super) async fn find(ctx: &Context, planned: &[Planned]) -> Result<Self> {
        for planned in planned {
            let Planned::Ending { checked, .. } = planned else {
                continue;
            };
            let Ending::Workspace(check) = &checked.space else {
                continue;
            };
            let escape = super::super::workspaces::escape_destination(ctx, &check.space).await?;
            if let Some(escape) = escape {
                return Ok(Self(Some((check.space.id.clone(), escape))));
            }
        }
        Ok(Self(None))
    }

    /// Send the shell out of the workspace once `report` says it ended.
    pub(super) fn navigate(self, ctx: &Context, report: &MoveReport) -> Result<()> {
        let Some((id, destination)) = self.0 else {
            return Ok(());
        };
        if report
            .moved
            .iter()
            .any(|moved| moved.id == id && moved.removal.is_some())
        {
            shell::navigate(ctx, &destination)?;
        }
        Ok(())
    }
}
