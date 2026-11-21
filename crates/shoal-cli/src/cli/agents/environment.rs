//! A space's `[env]` variables, as its agents receive them.
use std::{collections::BTreeMap, ffi::OsString};

use crate::{cli::context::Context, model::Space};

/// The [`shoal_workers::settings::configured_environment`] of `space`, a
/// workspace or a chat, for finding its agents' files, or none with a
/// warning when it cannot be resolved.
pub(super) async fn agent_environment(ctx: &Context, space: &Space) -> BTreeMap<String, OsString> {
    shoal_workers::settings::configured_environment(&ctx.paths, space)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "warning: cannot resolve the [env] of {} for its agents: {error:#}",
                space.name
            );
            Default::default()
        })
}
