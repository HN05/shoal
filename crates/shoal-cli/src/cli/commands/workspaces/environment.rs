//! A workspace's `[env]` variables, as its agents receive them.
use std::{collections::BTreeMap, ffi::OsString};

use crate::{cli::context::Context, model::Workspace};

/// The workspace's [`shoal_workers::settings::configured_environment`] for finding its agents' files,
/// or none with a warning when it cannot be resolved.
pub(in crate::cli) async fn agent_environment(
    ctx: &Context,
    workspace: &Workspace,
) -> BTreeMap<String, OsString> {
    shoal_workers::settings::configured_environment(&ctx.paths, workspace)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "warning: cannot resolve the [env] of {} for its agents: {error:#}",
                workspace.name
            );
            Default::default()
        })
}
