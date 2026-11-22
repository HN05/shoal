//! CLI requests for a workspace's service containers, and their rendering.
use anyhow::Result;

use crate::client::output::leases::Suggested;
use crate::{
    cli::{
        args::SpaceFilter,
        context::{Context, optional},
        output::{Palette, Style},
        ui,
    },
    client::leases::{self, ContainerAcquisition},
};
use shoal_protocol::containers::{ContainerCatalog, ContainerOverview, Containers};
use shoal_protocol::space_names::SpaceSelector;

use super::workspace_overviews;

/// Start the workspace's containers. Exit 2 means a port they reserve was
/// not: it waits for approval, or its preferred port is taken.
pub(super) async fn acquire(
    ctx: &Context,
    workspace: SpaceSelector,
    reason: Option<String>,
) -> Result<i32> {
    match leases::acquire_containers(&ctx.paths, workspace, reason).await? {
        ContainerAcquisition::Started(containers) => {
            let text = describe(&containers, Palette::stdout(ctx.json));
            ctx.emit_styled(Style::Success, &text, &*containers)?;
            Ok(0)
        }
        ContainerAcquisition::Approval(request) => super::approvals::declined(ctx, &request),
        ContainerAcquisition::Suggested(proposal) => {
            let text = format!(
                "{}: port {} unavailable; reserve the suggested one with \
                 `shoal acquire port {} --port {}`, then acquire the containers again",
                proposal.name, proposal.requested_port, proposal.name, proposal.suggested_port
            );
            let suggested = Suggested {
                suggestion: proposal,
                reserved: false,
            };
            ctx.emit_styled(Style::Warning, &text, suggested)?;
            Ok(super::EXIT_BUSY)
        }
    }
}

pub(super) async fn overview(ctx: &Context, filter: SpaceFilter) -> Result<i32> {
    let Some(workspace) =
        ui::select_workspace_filter(ctx, filter, &crate::cli::ui::Acting::Unconverted).await?
    else {
        return workspace_overviews(
            ctx,
            async |workspace| fetch_overview(ctx, &SpaceSelector::id(&workspace.id)).await,
            render_overview,
        )
        .await;
    };
    let overview = fetch_overview(ctx, &workspace).await?;
    ctx.show(&overview, |overview| {
        render_overview(overview, Palette::stdout(ctx.json))
    })?;
    Ok(0)
}

pub(super) async fn fetch_overview(
    ctx: &Context,
    workspace: &SpaceSelector,
) -> Result<ContainerOverview> {
    leases::containers(&ctx.paths, Some(workspace.clone())).await
}

pub(super) fn render_overview(overview: &ContainerOverview, palette: Palette) {
    for containers in &overview.projects {
        outln!("{}", describe(containers, palette));
    }
    if overview.projects.is_empty() {
        outln!("No containers");
    }
}

pub(super) async fn fetch_catalog(
    ctx: &Context,
    workspace: &SpaceSelector,
) -> Result<ContainerCatalog> {
    leases::container_catalog(&ctx.paths, workspace.clone()).await
}

pub(super) fn render_catalog(catalog: &ContainerCatalog, palette: Palette) {
    let Some(compose) = &catalog.compose else {
        outln!("No Compose file configured");
        return;
    };
    let ports = match catalog.ports.as_slice() {
        [] => String::new(),
        ports => format!(" on ports {}", ports.join(", ")),
    };
    outln!(
        "{}{ports}; starts on {}",
        palette.paint(Style::Heading, compose),
        catalog.start
    );
    match (&catalog.unavailable, catalog.runtime) {
        (Some(reason), _) => outln!(
            "{}",
            palette.paint(Style::Warning, format!("Runtime unavailable: {reason}"))
        ),
        (None, Some(runtime)) => outln!("Runtime: {runtime}"),
        (None, None) => {}
    }
}

/// The project with its state and who acquired it, then a line per service
/// with the ports it published.
fn describe(containers: &Containers, palette: Palette) -> String {
    let project = &containers.project;
    let mut lines = vec![format!(
        "{}  {}  {}  acquired by {}{}",
        palette.paint(Style::Heading, &project.project),
        project.state,
        project.runtime,
        project.acquirer.kind(),
        optional(project.reason.as_deref(), |reason| format!("  {reason}"))
    )];
    if let Some(error) = &project.error {
        lines.push(
            palette
                .paint(Style::Warning, format!("  {error}"))
                .to_string(),
        );
    }
    for service in &containers.services {
        let published: Vec<_> = (service.ports.iter())
            .map(|port| format!("{}->{}/{}", port.published, port.target, port.protocol))
            .collect();
        lines.push(format!(
            "  {}  {}  {}",
            service.service,
            service.state,
            published.join(" ")
        ));
    }
    if let Some(error) = &containers.services_error {
        let unlisted = format!("  services unavailable: {error}");
        lines.push(palette.paint(Style::Warning, unlisted).to_string());
    }
    lines.join("\n")
}
