//! Spaces of either kind: every space the caller sees.
use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::{
    model::{Space, Spaces},
    protocol::WorkspaceMethod,
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};
use std::collections::BTreeMap;

/// The workspaces and chats, running or stopped, visible to this caller;
/// the daemon filters scoped requests.
pub async fn list(daemon: &impl Daemon) -> Result<Spaces> {
    let spaces: Vec<Space> = request(daemon, WorkspaceMethod::ListSpaces).await?;
    Ok(Spaces(spaces))
}

/// A new scope token that acts for `space` until [`revoke_scope`] revokes
/// it or the space is removed.
pub async fn issue_scope(daemon: &impl Daemon, space: SpaceSelector) -> Result<ScopeToken> {
    request(daemon, WorkspaceMethod::IssueScope { space }).await
}

/// Revoke `token`, which [`issue_scope`] issued for `space`.
pub async fn revoke_scope(
    daemon: &impl Daemon,
    space: SpaceSelector,
    token: ScopeToken,
) -> Result<()> {
    request(daemon, WorkspaceMethod::RevokeScope { space, token }).await
}

/// The variables `space` gives its processes, without a scope token, as a
/// shell sets them: a preset's directories go first on the configured PATH,
/// or else on `inherited`.
pub async fn environment(
    daemon: &impl Daemon,
    space: SpaceSelector,
    inherited: Option<std::ffi::OsString>,
) -> Result<BTreeMap<String, String>> {
    let values = request(daemon, WorkspaceMethod::SpaceEnvironment { space }).await?;
    shoal_protocol::env::export_configured(values, inherited)
}
