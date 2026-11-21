//! Spaces of either kind: every space the caller sees.
use anyhow::Result;

use crate::connection::{Daemon, pages, request};
use shoal_protocol::{
    model::{Everywhere, Located, Space, Spaces},
    protocol::SpaceMethod,
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
};
use std::collections::BTreeMap;

/// The workspaces and chats, running or stopped, visible to this caller;
/// the daemon filters scoped requests.
pub async fn list(daemon: &impl Daemon) -> Result<Spaces> {
    let spaces: Vec<Space> = request(daemon, SpaceMethod::ListSpaces).await?;
    Ok(Spaces(spaces))
}

/// The spaces the caller sees on every machine: this machine's, unless not
/// `here`, and the other joined machines' as the hub lists them, or why the
/// hub could not be asked.
pub async fn everywhere(daemon: &impl Daemon, here: bool) -> Result<Everywhere> {
    let method = SpaceMethod::ListSpacesEverywhere { here };
    Everywhere::from_parts(pages(daemon, method).await?)
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
pub async fn issue_scope(daemon: &impl Daemon, space: SpaceSelector) -> Result<ScopeToken> {
    request(daemon, SpaceMethod::IssueScope { space }).await
}

/// Revoke `token`, which [`issue_scope`] issued for `space`.
pub async fn revoke_scope(
    daemon: &impl Daemon,
    space: SpaceSelector,
    token: ScopeToken,
) -> Result<()> {
    request(daemon, SpaceMethod::RevokeScope { space, token }).await
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
