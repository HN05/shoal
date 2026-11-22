//! A space's `[env]` variables, as its agents receive them.
use std::{collections::BTreeMap, ffi::OsString};

use crate::{client::connection::CommandPaths, model::Space};

/// The [`crate::settings::configured_environment`] of `space`, a
/// workspace or a chat, for finding its agents' files, or none with a
/// warning when it cannot be resolved.
pub(super) async fn agent_environment(
    paths: &CommandPaths,
    space: &Space,
) -> BTreeMap<String, OsString> {
    crate::settings::configured_environment(paths, space)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "warning: cannot resolve the [env] of {} for its agents: {error:#}",
                space.name
            );
            Default::default()
        })
}
