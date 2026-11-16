//! What a workspace saved for `shoal restore` when its agents and commands
//! stopped, which the daemon reads from its state, and how a resume
//! restores it.
use shoal_protocol::saved::{Saved, SavedSummary};

use anyhow::{Context as _, Result};

use crate::connection::{Daemon, pages};
use shoal_protocol::{
    protocol::{RecoveryMethod, SavedPiece},
    space_names::SpaceSelector,
};

/// What the existing space `space` saved, reassembled from the pieces of
/// JSON the daemon sends it in.
pub async fn read(daemon: &impl Daemon, space: SpaceSelector) -> Result<Saved> {
    let pieces = pages::<SavedPiece>(daemon, RecoveryMethod::Saved { space }).await?;
    let json: String = pieces.into_iter().map(|piece| piece.0).collect();
    serde_json::from_str(&json).context("the daemon sent invalid saved work")
}

/// What each space this caller sees saved, in brief, for those that saved
/// anything, in one read for listings and pickers.
pub async fn summaries(daemon: &impl Daemon) -> Result<Vec<SavedSummary>> {
    pages(daemon, RecoveryMethod::SavedSummaries).await
}

/// Whether a space this caller sees has stopped work that waits for `shoal
/// resume`.
pub async fn waits_for_resume(daemon: &impl Daemon) -> Result<bool> {
    let summaries = summaries(daemon).await?;
    Ok(summaries.iter().any(|summary| summary.restorable))
}
