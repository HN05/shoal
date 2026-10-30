//! CLI requests and rendering for cooperative resources.
use anyhow::{Result, ensure};
use serde_json::json;

use super::{
    acquisition::{Acquisition, obtain},
    workspace_overviews,
};
use crate::{
    cli::{
        args::WorkspaceScope,
        client::{self, request},
        context::{Context, optional},
        output::{Palette, Style},
        ui,
    },
    daemon::resources::{
        Catalog, Overview, ResourceKind, ResourceLease, ResourceRequest, WorkspaceOverview,
    },
    protocol::ResourceMethod,
};

/// Exit 2 means no compatible capacity or a pending or denied approval; a
/// queued request returns only once acquired or denied.
pub(super) async fn acquire(
    ctx: &Context,
    workspace: String,
    request: ResourceRequest,
) -> Result<i32> {
    let outcome = obtain(
        ctx,
        &format!("pool {}", request.pool),
        request.queue,
        async || {
            let method = ResourceMethod::ResourceAcquire {
                workspace: workspace.clone(),
                request: request.clone(),
            };
            client::request::<Acquisition<ResourceLease>>(&ctx.paths, method).await
        },
    )
    .await?;
    outcome.finish(
        ctx,
        |lease| ctx.emit(&describe(&lease, Palette::stdout(ctx.json)), &lease),
        |message| {
            json!({
                "acquired": false,
                "code": "resource_busy",
                "pool": request.pool,
                "resource": request.resource,
                "message": message,
            })
        },
    )
}

fn describe(lease: &ResourceLease, palette: Palette) -> String {
    format!(
        "{}/{} -> {} [{}] ({}){}",
        palette.paint(Style::Heading, &lease.pool),
        lease.name,
        lease.resource,
        lease.mode,
        lease.id,
        repository_path(lease)
    )
}

fn describe_short(lease: &ResourceLease, palette: Palette) -> String {
    format!(
        "{}/{} -> {} [{}]{}{}",
        palette.paint(Style::Heading, &lease.pool),
        lease.name,
        lease.resource,
        lease.mode,
        optional(lease.reason.as_deref(), |r| format!(" ({r})")),
        repository_path(lease)
    )
}

pub(super) fn repository_path(lease: &ResourceLease) -> String {
    lease
        .repository
        .as_ref()
        .map_or_else(String::new, |repo| format!(" -> {}", repo.path.display()))
}

/// Which pool members an overview shows: related repositories are listed
/// apart from permits and locks.
#[derive(Clone, Copy)]
pub(super) enum Members {
    All,
    Locks,
    Repositories,
}

impl Members {
    fn includes(self, kind: ResourceKind) -> bool {
        match self {
            Self::All => true,
            Self::Locks => kind != ResourceKind::Repo,
            Self::Repositories => kind == ResourceKind::Repo,
        }
    }

    fn retain_catalog(self, catalog: &mut Catalog) {
        for pool in &mut catalog.pools {
            pool.definition
                .resources
                .retain(|_, resource| self.includes(resource.kind));
        }
        catalog
            .pools
            .retain(|pool| !pool.definition.resources.is_empty());
    }

    pub(super) fn retain(self, overview: &mut Overview) {
        for pool in &mut overview.pools {
            pool.resources
                .retain(|resource| self.includes(resource.kind));
        }
        overview.pools.retain(|pool| !pool.resources.is_empty());
        overview.leases.retain(|lease| match self {
            Self::All => true,
            Self::Locks => lease.repository.is_none(),
            Self::Repositories => lease.repository.is_some(),
        });
    }
}

pub(super) async fn fetch_overview(
    ctx: &Context,
    workspace: String,
    members: Members,
) -> Result<Overview> {
    let mut overview =
        request::<Overview>(&ctx.paths, ResourceMethod::ResourceOverview { workspace }).await?;
    members.retain(&mut overview);
    Ok(overview)
}

pub(super) async fn fetch_catalog(
    ctx: &Context,
    workspace: &str,
    members: Members,
) -> Result<Catalog> {
    let method = ResourceMethod::ResourceCatalog {
        workspace: workspace.to_owned(),
    };
    let mut catalog = request::<Catalog>(&ctx.paths, method).await?;
    members.retain_catalog(&mut catalog);
    Ok(catalog)
}

pub(super) fn render_catalog(catalog: &Catalog, palette: Palette) {
    for pool in &catalog.pools {
        outln!(
            "{} ({}) capacity {}{}",
            palette.paint(Style::Heading, &pool.name),
            pool.scope,
            pool.definition.capacity,
            optional(pool.definition.reason.as_deref(), |r| format!("  {r}"))
        );
        for (name, resource) in &pool.definition.resources {
            outln!(
                "  {name}: {}, capacity {}{}{}",
                resource.kind,
                resource.capacity,
                optional(resource.repo.as_deref(), |repo| format!(
                    ", repository {repo}"
                )),
                if resource.requires_approval {
                    format!("; approval required ({})", resource.approval_lifetime)
                } else {
                    String::new()
                }
            );
        }
    }
    if catalog.pools.is_empty() {
        outln!("No configured resources");
    }
}

/// Choose a configured pool with `members` for the workspace, standing in
/// for the command's `argument`.
pub(super) async fn pick_pool(
    ctx: &Context,
    workspace: &str,
    members: Members,
    argument: &str,
) -> Result<String> {
    let overview = fetch_overview(ctx, workspace.to_owned(), members).await?;
    ensure!(!overview.pools.is_empty(), "no configured resources");
    ui::require_argument(ctx, "resource", argument)?;
    let entries = overview
        .pools
        .into_iter()
        .map(|pool| {
            let label = format!("{} ({})", pool.name, pool.scope);
            (pool.name, label)
        })
        .collect();
    ui::pick(ctx, "Resource> ", entries)
}

pub(super) async fn overview(
    ctx: &Context,
    scope: WorkspaceScope,
    members: Members,
) -> Result<i32> {
    let Some(workspace) = ui::select_workspace_filter(ctx, scope).await? else {
        return workspace_overviews(
            ctx,
            async |workspace| {
                let overview = fetch_overview(ctx, workspace.id.clone(), members).await?;
                Ok(WorkspaceOverview {
                    workspace: workspace.clone(),
                    overview,
                })
            },
            |overview, palette| render_overview(&overview.overview, palette),
        )
        .await;
    };
    let overview = fetch_overview(ctx, workspace, members).await?;
    ctx.show(&overview, |overview| {
        render_overview(overview, Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

pub(super) fn render_overview(overview: &Overview, palette: Palette) {
    for pool in &overview.pools {
        outln!(
            "{} ({}) {}/{} in use, {} available{}",
            palette.paint(Style::Heading, &pool.name),
            pool.scope,
            pool.used,
            pool.capacity,
            pool.available,
            if pool.configuration_matches {
                String::new()
            } else {
                palette.paint(
                    Style::Warning,
                    " [configuration changed; drain leases first]",
                )
            }
        );
        for resource in &pool.resources {
            if resource.kind == ResourceKind::Repo {
                outln!(
                    "  {}: {} readers; read available: {}",
                    resource.name,
                    resource.readers,
                    resource.read_available
                );
            } else if resource.kind == ResourceKind::Rwlock {
                outln!(
                    "  {}: {} readers, {} writers; read available: {}, write available: {}",
                    resource.name,
                    resource.readers,
                    resource.writers,
                    resource.read_available,
                    resource.write_available
                );
            } else {
                outln!(
                    "  {}: {}/{} in use, {} available",
                    resource.name,
                    resource.used,
                    resource.capacity,
                    resource.available
                );
            }
        }
    }
    for lease in &overview.leases {
        outln!("  lease {}", describe_short(lease, palette));
    }
    if overview.pools.is_empty() && overview.leases.is_empty() {
        outln!("No configured resources or leases");
    }
}
