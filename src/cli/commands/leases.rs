//! Typed acquisition and release across ports, simulators, and resources.
use anyhow::Result;

use super::{ports, resources, simulators};
use crate::{
    cli::{
        AcquireKind, LeaseOptions,
        context::Context,
        ui::{self, Fallback},
    },
    daemon::{
        ports::PortRequest,
        resources::{LockMode, ResourceKind, ResourceRequest},
    },
    sim::SimRequest,
};

pub(super) async fn acquire(
    ctx: &Context,
    kind: AcquireKind,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    match kind {
        AcquireKind::Port {
            name,
            port,
            env,
            reason,
            on_conflict,
        } => {
            let request = PortRequest {
                port,
                env_var: env,
                reason,
                on_conflict,
            };
            ports::acquire(ctx, workspace, name, request).await
        }
        AcquireKind::Sim {
            lease,
            profile,
            device,
            runtime,
            clean,
        } => {
            let request = SimRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                clean,
                name: lease.name,
                profile,
                device,
                runtime,
                reason: lease.reason,
            };
            simulators::acquire(ctx, workspace, request, lease.wait).await
        }
        AcquireKind::Resource {
            pool,
            member,
            mode,
            lease,
        } => {
            let request = resource_request(pool, member, mode, None, &lease);
            resources::acquire(ctx, workspace, request, lease.wait).await
        }
        AcquireKind::Repo { resource, lease } => {
            let request = resource_request(
                resource,
                None,
                Some(LockMode::Read),
                Some(ResourceKind::Repo),
                &lease,
            );
            resources::acquire(ctx, workspace, request, lease.wait).await
        }
    }
}

fn resource_request(
    pool: String,
    member: Option<String>,
    mode: Option<LockMode>,
    kind: Option<ResourceKind>,
    lease: &LeaseOptions,
) -> ResourceRequest {
    ResourceRequest {
        mode,
        kind,
        pool,
        name: lease.name.clone(),
        resource: member,
        reason: lease.reason.clone(),
    }
}
