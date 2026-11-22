//! Typed acquisition and release across ports, simulators, containers and
//! resources.

use anyhow::{Context as _, Result, ensure};

use super::{
    containers, ports,
    resources::{self, Members},
    secrets, simulators, workspace_overviews,
};
use crate::client::output::leases::{Held, Leases, Releases};
use crate::{
    cli::{
        args::{
            SpaceFilter,
            resources::{AcquireKind, LeaseKind, LeaseOptions, PoolSelection, ReleaseKind},
        },
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::leases,
    model::PortRequest,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    approvals::{ApprovalRequest, Specification, Target},
    resources::{LockMode, ResourceKind, ResourceRequest},
    sim::SimRequest,
};

pub(super) async fn acquire(
    ctx: &Context,
    kind: AcquireKind,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let holds = matches!(kind, AcquireKind::Hold { .. });
    let workspace = select_target(ctx, holds, space).await?;
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
                clean,
                profile,
                device,
                runtime,
                reason: lease.reason,
                queue: lease.queue,
                ..SimRequest::new(lease.name)
            };
            simulators::acquire(ctx, workspace, request).await
        }
        AcquireKind::Containers { reason } => containers::acquire(ctx, workspace, reason).await,
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

/// The space a lease goes to, which `explicit` names or the current space
/// or a picker chooses: any space for a hold, `holds`, which a chat takes
/// too, else a workspace, as a lease of its worktree.
async fn select_target(
    ctx: &Context,
    holds: bool,
    explicit: Option<SpaceSelector>,
) -> Result<SpaceSelector> {
    match holds {
        true => {
            ui::select_space(
                ctx,
                explicit,
                Fallback::CurrentDirectory(&crate::cli::ui::Acting::Unconverted),
            )
            .await
        }
        false => {
            ui::select_workspace(
                ctx,
                explicit,
                Fallback::CurrentDirectory(&crate::cli::ui::Acting::Unconverted),
            )
            .await
        }
    }
}

/// Release the selected leases in order; a failure leaves the rest held.
pub(super) async fn release(
    ctx: &Context,
    kind: Option<ReleaseKind>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let holds = matches!(kind, Some(ReleaseKind::Hold { .. }));
    let workspace = select_target(ctx, holds, space).await?;
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
    workspace: &SpaceSelector,
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
        Some(ReleaseKind::Containers) => return Ok(vec![Held::Containers]),
        _ => {}
    }
    let inspection = crate::client::workspaces::inspect(&ctx.paths, workspace.clone()).await?;
    let id = inspection.workspace.id.clone();
    let ports = inspection
        .ports
        .into_iter()
        .map(|port| Held::Port { name: port.name });
    let simulators = inspection
        .simulators
        .into_iter()
        .filter(|sim| sim.workspace_id.as_ref() == Some(&inspection.workspace.id))
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
    let containers = leases::containers(&ctx.paths, Some(workspace.clone())).await?;
    let containers = (containers.projects.into_iter())
        .filter(|containers| containers.project.state.active())
        .map(|_| Held::Containers);
    let requests = crate::client::approvals::list(&ctx.paths, Some(workspace.clone()))
        .await?
        .into_iter()
        .filter(|request| request.space_id == id && request.active)
        .filter_map(requested);
    let mut selected = Vec::new();
    // Containers stop before the ports they bind go back to the pool.
    let held = containers.chain(ports).chain(simulators).chain(resources);
    for held in held.chain(requests) {
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
/// and unmerge requests, proposals, push requests, hold requests and
/// requests to take an action belong to what they ask for, not to a lease.
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
            Target::Merge
            | Target::Unmerge
            | Target::IssueOpen
            | Target::PrOpen
            | Target::Push
            | Target::Hold(_)
            | Target::Action(_),
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
        | (Held::Containers, Some(ReleaseKind::Containers))
        | (Held::Secret { .. }, Some(ReleaseKind::Secret { .. })) => true,
        (Held::Resource { pool, name }, Some(ReleaseKind::Resource { selection }))
        | (Held::Repo { pool, name }, Some(ReleaseKind::Repo { selection })) => {
            selects(pool, name, selection)
        }
        _ => false,
    }
}

async fn release_one(ctx: &Context, workspace: &SpaceSelector, held: &Held) -> Result<()> {
    let workspace = workspace.to_owned();
    match held {
        Held::Port { name } => {
            leases::release_port(&ctx.paths, workspace.clone(), name.clone()).await
        }
        Held::Sim { name } => {
            leases::release_simulator(&ctx.paths, workspace.clone(), name.clone()).await
        }
        Held::Containers => leases::release_containers(&ctx.paths, workspace.clone()).await,
        Held::Resource { pool, name } | Held::Repo { pool, name } => {
            leases::release_resource(&ctx.paths, workspace.clone(), pool.clone(), name.clone())
                .await
        }
        Held::Secret { name } => {
            leases::release_secret(&ctx.paths, workspace.clone(), name.clone()).await
        }
    }
}

pub(super) async fn show(
    ctx: &Context,
    kind: Option<LeaseKind>,
    filter: SpaceFilter,
) -> Result<i32> {
    match kind {
        Some(LeaseKind::Hold) => super::holds::list(ctx, filter).await,
        Some(LeaseKind::Port) => ports::overview(ctx, filter).await,
        Some(LeaseKind::Sim) => simulators::overview(ctx, filter).await,
        Some(LeaseKind::Containers) => containers::overview(ctx, filter).await,
        Some(LeaseKind::Resource) => resources::overview(ctx, filter, Members::Locks).await,
        Some(LeaseKind::Repo) => resources::overview(ctx, filter, Members::Repositories).await,
        None => {
            let Some(workspace) =
                ui::select_workspace_filter(ctx, filter, &crate::cli::ui::Acting::Unconverted)
                    .await?
            else {
                return workspace_overviews(
                    ctx,
                    async |workspace| fetch_leases(ctx, &SpaceSelector::id(&workspace.id)).await,
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

async fn fetch_leases(ctx: &Context, workspace: &SpaceSelector) -> Result<Leases> {
    let ports = leases::ports(&ctx.paths, workspace.clone()).await?;
    let simulators = leases::simulators(&ctx.paths, Some(workspace.clone())).await?;
    let containers = containers::fetch_overview(ctx, workspace).await?;
    let resources = resources::fetch_overview(ctx, workspace.to_owned(), Members::All).await?;
    Ok(Leases {
        workspace: ports.workspace.clone(),
        ports,
        simulators,
        containers,
        resources,
    })
}

/// Simulators and containers appear only where they exist.
fn render_leases(leases: &Leases, palette: Palette) {
    outln!("{}", palette.paint(Style::Heading, "Ports"));
    ports::render_overview(&leases.ports, palette);
    let simulators = &leases.simulators;
    if !simulators.simulators.is_empty() {
        outln!("{}", palette.paint(Style::Heading, "Simulators"));
        simulators::render_overview(simulators, palette);
    }
    if !leases.containers.projects.is_empty() {
        outln!("{}", palette.paint(Style::Heading, "Containers"));
        containers::render_overview(&leases.containers, palette);
    }
    outln!("{}", palette.paint(Style::Heading, "Resources"));
    resources::render_overview(&leases.resources, palette);
}
