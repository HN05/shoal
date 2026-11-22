//! Spaces of either kind: finding, listing and naming them, where each
//! runs, its status and environment, and its scope tokens.
use anyhow::Result;

use crate::connection::{Daemon, Prepared, pages, request};
use shoal_protocol::{
    model::{Everywhere, Located, RenamedSpace, Space, SpaceTarget, Spaces},
    protocol::{Operation, SpaceMethod},
    scope_tokens::ScopeToken,
    space_names::{SpaceName, SpaceSelector},
    status::StatusAnswer,
};
use std::collections::BTreeMap;

/// The workspaces and chats, running or stopped, visible to this caller;
/// the daemon filters scoped requests.
pub async fn list(daemon: &impl Daemon) -> Result<Spaces> {
    let spaces: Vec<Space> = request(daemon, SpaceMethod::ListSpaces).await?;
    Ok(Spaces(spaces))
}

/// The workspace or chat `selector` names, by number, ID or name.
pub async fn present_space(
    daemon: &impl Daemon,
    selector: &SpaceSelector,
) -> Result<Option<Space>> {
    Ok(list(daemon).await?.find(selector))
}

/// The spaces the caller sees on every machine: this machine's, unless not
/// `here`, and the other joined machines' as the hub lists them, or why the
/// hub could not be asked.
pub async fn everywhere(daemon: &impl Daemon, here: bool) -> Result<Everywhere> {
    let method = SpaceMethod::ListSpacesEverywhere { here, acting: None };
    Everywhere::from_parts(pages(daemon, method).await?)
}

/// The other joined machines' spaces the caller sees that `acting`, a
/// request a picker offers spaces for, can act on, or why the hub could
/// not be asked; none when it stays on this machine.
pub async fn offered_elsewhere(daemon: &impl Daemon, acting: Operation) -> Result<Everywhere> {
    let acting = Some(Box::new(acting));
    let method = SpaceMethod::ListSpacesEverywhere {
        here: false,
        acting,
    };
    Everywhere::from_parts(pages(daemon, method).await?)
}

/// What stands for the space a picker is about to pick in the request it
/// offers spaces for, which [`offered_elsewhere`] judges by what it does
/// and never resolves.
pub fn picking() -> SpaceSelector {
    SpaceSelector::Number(shoal_ids::space_ids::SpaceNumber::new(0))
}

/// The spaces `spaces` name, or with `machines` every space of those joined
/// machines, with those below each on every machine when `below`, each
/// after the space above it and with where it runs.
pub async fn select(
    daemon: &impl Daemon,
    spaces: Vec<SpaceSelector>,
    machines: Vec<String>,
    below: bool,
) -> Result<Vec<Located>> {
    let method = SpaceMethod::SelectSpaces {
        spaces,
        machines,
        below,
    };
    request(daemon, method).await
}

/// A new scope token that acts for `space` until [`revoke_scope`] revokes
/// it or the space is removed.
pub fn issue_scope(space: SpaceSelector) -> Prepared<ScopeToken> {
    Prepared::new(SpaceMethod::IssueScope { space })
}

/// Revoke `token`, which [`issue_scope`] issued for `space`.
pub fn revoke_scope(space: SpaceSelector, token: ScopeToken) -> Prepared<()> {
    Prepared::new(SpaceMethod::RevokeScope { space, token })
}

/// The variables `space` gives its processes, without a scope token, as a
/// shell sets them: a preset's directories go first on the configured PATH,
/// or else on `inherited`.
pub async fn environment(
    daemon: &impl Daemon,
    space: SpaceSelector,
    inherited: Option<std::ffi::OsString>,
) -> Result<BTreeMap<String, String>> {
    let values = request(daemon, SpaceMethod::SpaceEnvironment { space }).await?;
    shoal_protocol::env::export_configured(values, inherited)
}

/// Give the workspace or chat `space` the name `name`.
pub fn rename_space(space: SpaceSelector, name: SpaceName) -> Prepared<RenamedSpace> {
    Prepared::new(SpaceMethod::RenameSpace { space, name })
}

/// Where the space `space` names runs: here, with its record, or on the
/// other joined machine the hub lists it on.
pub async fn locate(daemon: &impl Daemon, space: SpaceSelector) -> Result<Located> {
    let located: Box<Located> = request(daemon, SpaceMethod::LocateSpace { space }).await?;
    Ok(*located)
}

/// The spaces `target` selects.
pub async fn find_spaces(daemon: &impl Daemon, target: SpaceTarget) -> Result<Vec<Space>> {
    request(daemon, SpaceMethod::FindSpaces { target }).await
}

/// What `space` is doing: its links, agents, leases and assignment, from
/// the machine that runs it, or what that machine last reported while it
/// is offline.
pub async fn status(daemon: &impl Daemon, space: SpaceSelector) -> Result<StatusAnswer> {
    request(daemon, SpaceMethod::SpaceStatus { space }).await
}
