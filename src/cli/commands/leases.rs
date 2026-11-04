//! Typed acquisition and release across ports, simulators, and resources.
use std::fmt;

use anyhow::{Context as _, Result, ensure};

use super::{ports, resources, resources::Members, secrets, simulators, workspace_overviews};
use crate::client::output::leases::{Held, Leases, Releases};
use crate::{
    cli::{
        args::{
            WorkspaceScope,
            resources::{AcquireKind, LeaseKind, LeaseOptions, PoolSelection, ReleaseKind},
        },
        client,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::{
        approvals::{ApprovalRequest, Specification, Target},
        leases,
        resources::{LockMode, ResourceKind, ResourceRequest},
    },
    model::{Inspection, PortRequest},
    protocol::WorkspaceMethod,
    sim::SimRequest,
};

pub(super) async fn acquire(
    ctx: &Context,
    kind: AcquireKind,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    match kind {
        AcquireKind::Hold { name, reason } => {
            super::holds::acquire(ctx, workspace, name, reason).await
        }
        AcquireKind::Port {
            name,
            port,
            env,
            reason,
            on_conflict,
        } => {
            let name = match name {
                Some(name) => name,
                None => ports::pick_configured(ctx, &workspace).await?,
            };
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
                queue: lease.queue,
            };
            simulators::acquire(ctx, workspace, request).await
        }
        AcquireKind::Resource {
            pool,
            member,
            mode,
            lease,
        } => {
            let pool = match pool {
                Some(pool) => pool,
                None => resources::pick_pool(ctx, &workspace, Members::Locks, "pool").await?,
            };
            let request = resource_request(pool, member, mode, None, &lease);
            resources::acquire(ctx, workspace, request).await
        }
        AcquireKind::Repo { resource, lease } => {
            let resource = match resource {
                Some(resource) => resource,
                None => {
                    resources::pick_pool(ctx, &workspace, Members::Repositories, "resource").await?
                }
            };
            let request = resource_request(
                resource,
                None,
                Some(LockMode::Read),
                Some(ResourceKind::Repo),
                &lease,
            );
            resources::acquire(ctx, workspace, request).await
        }
        AcquireKind::Secret {
            name,
            reason,
            queue,
        } => secrets::acquire(ctx, workspace, name, reason, queue).await,
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
        queue: lease.queue,
    }
}

impl fmt::Display for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Port { name } => write!(f, "port {name}"),
            Self::Sim { name } => write!(f, "simulator {name}"),
            Self::Resource { pool, name } => write!(f, "resource {pool}/{name}"),
            Self::Repo { pool, name } => write!(f, "repo {pool}/{name}"),
            Self::Secret { name } => write!(f, "secret {name}"),
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
    if let Some(ReleaseKind::Hold { name }) = kind {
        return super::holds::release(ctx, workspace, name).await;
    }
    let selected = select_held(ctx, &workspace, kind).await?;
    ensure!(!selected.is_empty(), "no leases or requests to release");
    for held in &selected {
        release_one(ctx, &workspace, held)
            .await
            .with_context(|| format!("release {held}"))?;
    }
    let text = selected
        .iter()
        .map(|held| format!("Released {held}"))
        .collect::<Vec<_>>()
        .join("\n");
    ctx.emit_styled(Style::Success, &text, Releases { released: selected })?;
    Ok(0)
}

/// A named port or simulator lease is released as named; anything else
/// expands to the workspace's current leases, so a named resource lease must
/// also be of the requested kind.
async fn select_held(
    ctx: &Context,
    workspace: &str,
    kind: Option<ReleaseKind>,
) -> Result<Vec<Held>> {
    match &kind {
        Some(ReleaseKind::Port { name: Some(name) }) => {
            return Ok(vec![Held::Port { name: name.clone() }]);
        }
        Some(ReleaseKind::Sim { name: Some(name) }) => {
            return Ok(vec![Held::Sim { name: name.clone() }]);
        }
        Some(ReleaseKind::Secret { name: Some(name) }) => {
            return Ok(vec![Held::Secret { name: name.clone() }]);
        }
        _ => {}
    }
    let inspection = client::request::<Inspection>(
        &ctx.paths,
        WorkspaceMethod::InspectWorkspace {
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
    let requests = crate::client::approvals::list(&ctx.paths, Some(workspace.to_owned()))
        .await?
        .into_iter()
        .filter(|request| request.active)
        .filter_map(requested);
    let mut selected = Vec::new();
    for held in ports.chain(simulators).chain(resources).chain(requests) {
        if matches_kind(&held, kind.as_ref()) && !selected.contains(&held) {
            selected.push(held);
        }
    }
    if let Some(ReleaseKind::Resource { selection } | ReleaseKind::Repo { selection }) = &kind
        && let (Some(pool), Some(lease)) = (&selection.pool, &selection.lease)
    {
        ensure!(
            !selected.is_empty(),
            "no {} lease or request {pool}/{lease} in this workspace",
            if matches!(kind, Some(ReleaseKind::Repo { .. })) {
                "repo"
            } else {
                "resource"
            }
        );
    }
    Ok(selected)
}

/// Release cancels a pending or denied request even without a lease. Merge
/// requests, proposals, push requests and hold requests belong to merging,
/// opening, pushing and holding, not to a lease.
fn requested(request: ApprovalRequest) -> Option<Held> {
    let name = request.name;
    Some(match (request.target, request.specification) {
        (Target::Port(_), _) => Held::Port { name },
        (Target::Simulator, _) => Held::Sim { name },
        (Target::Resource { pool, .. }, Specification::Resource(bound))
            if bound.repository.is_some() =>
        {
            Held::Repo { pool, name }
        }
        (Target::Resource { pool, .. }, _) => Held::Resource { pool, name },
        (Target::Secret(_), _) => Held::Secret { name },
        (
            Target::Merge | Target::IssueOpen | Target::PrOpen | Target::Push | Target::Hold(_),
            _,
        ) => {
            return None;
        }
    })
}

fn matches_kind(held: &Held, kind: Option<&ReleaseKind>) -> bool {
    let selects = |pool: &str, name: &str, selection: &PoolSelection| {
        selection
            .pool
            .as_deref()
            .is_none_or(|wanted| wanted == pool)
            && selection
                .lease
                .as_deref()
                .is_none_or(|wanted| wanted == name)
    };
    match (held, kind) {
        (_, None)
        | (Held::Port { .. }, Some(ReleaseKind::Port { .. }))
        | (Held::Sim { .. }, Some(ReleaseKind::Sim { .. }))
        | (Held::Secret { .. }, Some(ReleaseKind::Secret { .. })) => true,
        (Held::Resource { pool, name }, Some(ReleaseKind::Resource { selection }))
        | (Held::Repo { pool, name }, Some(ReleaseKind::Repo { selection })) => {
            selects(pool, name, selection)
        }
        _ => false,
    }
}

async fn release_one(ctx: &Context, workspace: &str, held: &Held) -> Result<()> {
    let workspace = workspace.to_owned();
    match held {
        Held::Port { name } => leases::release_port(&ctx.paths, workspace, name.clone()).await,
        Held::Sim { name } => leases::release_simulator(&ctx.paths, workspace, name.clone()).await,
        Held::Resource { pool, name } | Held::Repo { pool, name } => {
            leases::release_resource(&ctx.paths, workspace, pool.clone(), name.clone()).await
        }
        Held::Secret { name } => leases::release_secret(&ctx.paths, workspace, name.clone()).await,
    }
}

pub(super) async fn show(
    ctx: &Context,
    kind: Option<LeaseKind>,
    scope: WorkspaceScope,
) -> Result<i32> {
    match kind {
        Some(LeaseKind::Hold) => super::holds::list(ctx, scope).await,
        Some(LeaseKind::Port) => ports::overview(ctx, scope).await,
        Some(LeaseKind::Sim) => simulators::overview(ctx, scope).await,
        Some(LeaseKind::Resource) => resources::overview(ctx, scope, Members::Locks).await,
        Some(LeaseKind::Repo) => resources::overview(ctx, scope, Members::Repositories).await,
        None => {
            let Some(workspace) = ui::select_workspace_filter(ctx, scope).await? else {
                return workspace_overviews(
                    ctx,
                    async |workspace| fetch_leases(ctx, &workspace.id).await,
                    render_leases,
                )
                .await;
            };
            let leases = fetch_leases(ctx, &workspace).await?;
            ctx.show(&leases, |leases| {
                render_leases(leases, Palette::stdout(ctx.json))
            })?;
            Ok(0)
        }
    }
}

async fn fetch_leases(ctx: &Context, workspace: &str) -> Result<Leases> {
    let ports = leases::ports(&ctx.paths, workspace.to_owned()).await?;
    let simulators = leases::simulators(&ctx.paths, Some(workspace.to_owned())).await?;
    let resources = resources::fetch_overview(ctx, workspace.to_owned(), Members::All).await?;
    Ok(Leases {
        workspace: ports.workspace.clone(),
        ports,
        simulators,
        resources,
    })
}

/// Simulators appear only where devices exist.
fn render_leases(leases: &Leases, palette: Palette) {
    outln!("{}", palette.paint(Style::Heading, "Ports"));
    ports::render_overview(&leases.ports, palette);
    let simulators = &leases.simulators;
    if !simulators.simulators.is_empty() {
        outln!("{}", palette.paint(Style::Heading, "Simulators"));
        simulators::render_overview(simulators, palette);
    }
    outln!("{}", palette.paint(Style::Heading, "Resources"));
    resources::render_overview(&leases.resources, palette);
}
