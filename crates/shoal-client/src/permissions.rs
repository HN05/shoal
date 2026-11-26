//! Reading the effective permissions of a space or a launch.
use anyhow::Result;

use crate::connection::{Daemon, request};
use shoal_protocol::{
    config::{
        modes::ModeName,
        permissions::{Permissions, PermissionsConfig},
    },
    protocol::PermissionMethod,
    repository::Selector,
    space_names::SpaceSelector,
};

/// The effective permissions of an agent starting in `mode` in `workspace`.
pub async fn launch_permissions(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    mode: ModeName,
    narrowing: PermissionsConfig,
) -> Result<Permissions> {
    let method = PermissionMethod::LaunchPermissions {
        workspace,
        mode,
        narrowing,
    };
    request(daemon, method).await
}

/// The effective permissions of this scoped process's own space for an
/// action in the registered repository `repository`.
pub async fn held_permissions(
    daemon: &impl Daemon,
    repository: Option<Selector>,
) -> Result<Permissions> {
    request(daemon, PermissionMethod::HeldPermissions { repository }).await
}
