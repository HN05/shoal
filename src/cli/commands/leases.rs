//! Typed acquisition and release across ports, simulators, and resources.
use std::fmt;

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::json;

use super::{ports, resources, simulators};
use crate::{
    cli::{
        AcquireKind, LeaseOptions, PoolSelection, ReleaseKind, client,
        context::Context,
        output::Style,
        ui::{self, Fallback},
    },
    daemon::{
        ports::PortRequest,
        resources::{LockMode, ResourceKind, ResourceRequest},
    },
    model::Inspection,
    protocol::Method,
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

/// One lease a workspace holds, named as its release request needs it.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Held {
    Port { name: String },
    Sim { name: String },
    Resource { pool: String, name: String },
    Repo { pool: String, name: String },
}

impl fmt::Display for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Port { name } => write!(f, "port {name}"),
            Self::Sim { name } => write!(f, "simulator {name}"),
            Self::Resource { pool, name } => write!(f, "resource {pool}/{name}"),
            Self::Repo { pool, name } => write!(f, "repo {pool}/{name}"),
        }
    }
}

/// Release the selected leases in order; a failure leaves the rest held.
pub(super) async fn release(
    ctx: &Context,
    kind: Option<ReleaseKind>,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let selected = select_held(ctx, &workspace, kind).await?;
    for held in &selected {
        release_one(ctx, &workspace, held)
            .await
            .with_context(|| format!("release {held}"))?;
    }
    let text = if selected.is_empty() {
        "No leases to release".to_owned()
    } else {
        selected
            .iter()
            .map(|held| format!("Released {held}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    ctx.emit_styled(Style::Success, &text, json!({ "released": selected }))?;
    Ok(0)
}

/// An explicit lease is released as named; a kind or no kind expands to the
/// workspace's current leases.
async fn select_held(
    ctx: &Context,
    workspace: &str,
    kind: Option<ReleaseKind>,
) -> Result<Vec<Held>> {
    let explicit = match &kind {
        Some(ReleaseKind::Port { name: Some(name) }) => Some(Held::Port { name: name.clone() }),
        Some(ReleaseKind::Sim { name: Some(name) }) => Some(Held::Sim { name: name.clone() }),
        Some(ReleaseKind::Resource {
            selection:
                PoolSelection {
                    pool: Some(pool),
                    lease: Some(name),
                },
        }) => Some(Held::Resource {
            pool: pool.clone(),
            name: name.clone(),
        }),
        Some(ReleaseKind::Repo {
            selection:
                PoolSelection {
                    pool: Some(pool),
                    lease: Some(name),
                },
        }) => Some(Held::Repo {
            pool: pool.clone(),
            name: name.clone(),
        }),
        _ => None,
    };
    if let Some(held) = explicit {
        return Ok(vec![held]);
    }
    let inspection = client::request::<Inspection>(
        &ctx.paths,
        Method::InspectWorkspace {
            workspace: workspace.to_owned(),
        },
    )
    .await?;
    let ports = inspection
        .ports
        .into_iter()
        .map(|port| Held::Port { name: port.name });
    let simulators = inspection
        .simulators
        .into_iter()
        .filter(|sim| sim.workspace_id.as_deref() == Some(&inspection.workspace.id))
        .filter_map(|sim| sim.lease_name.map(|name| Held::Sim { name }));
    let resources = inspection.resources.into_iter().map(|lease| {
        if lease.repository.is_some() {
            Held::Repo {
                pool: lease.pool,
                name: lease.name,
            }
        } else {
            Held::Resource {
                pool: lease.pool,
                name: lease.name,
            }
        }
    });
    Ok(ports
        .chain(simulators)
        .chain(resources)
        .filter(|held| matches_kind(held, kind.as_ref()))
        .collect())
}

fn matches_kind(held: &Held, kind: Option<&ReleaseKind>) -> bool {
    let in_pool = |pool: &str, selection: &PoolSelection| {
        selection
            .pool
            .as_deref()
            .is_none_or(|wanted| wanted == pool)
    };
    match (held, kind) {
        (_, None)
        | (Held::Port { .. }, Some(ReleaseKind::Port { .. }))
        | (Held::Sim { .. }, Some(ReleaseKind::Sim { .. })) => true,
        (Held::Resource { pool, .. }, Some(ReleaseKind::Resource { selection }))
        | (Held::Repo { pool, .. }, Some(ReleaseKind::Repo { selection })) => {
            in_pool(pool, selection)
        }
        _ => false,
    }
}

async fn release_one(ctx: &Context, workspace: &str, held: &Held) -> Result<()> {
    let workspace = workspace.to_owned();
    let method = match held {
        Held::Port { name } => Method::PortRelease {
            workspace,
            name: name.clone(),
        },
        Held::Sim { name } => Method::SimRelease {
            workspace,
            name: name.clone(),
        },
        Held::Resource { pool, name } | Held::Repo { pool, name } => Method::ResourceRelease {
            workspace,
            pool: pool.clone(),
            name: name.clone(),
        },
    };
    client::request::<()>(&ctx.paths, method).await
}
