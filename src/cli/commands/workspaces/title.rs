//! Titling a workspace, which the places Shoal opened for it show, or
//! asking its agent to.
use anyhow::{Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::turns,
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    model::RetitledWorkspace,
    protocol::ConfigTarget,
};

/// What `shoal work rename --request` asks the workspace's agent.
const TITLE_REQUEST: &str = "The user asks you to retitle this workspace: name the work as it stands now in two to five words with `shoal work rename \"<title>\"`.";

pub(in crate::cli::commands) async fn title(
    ctx: &Context,
    workspace: Option<String>,
    title: String,
) -> Result<i32> {
    // Refuse an invalid title before a picker asks for the workspace.
    crate::validate::title(title.trim())?;
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let retitled =
        crate::client::workspaces::set_title(&ctx.paths, SpaceSelector::parse(&workspace), title)
            .await?;
    // The title is recorded; a place left with its old label only warns.
    if let Err(error) = relabel(ctx, &retitled).await {
        eprintln!(
            "warning: cannot relabel the places of {}: {error:#}",
            retitled.workspace.name
        );
    }
    ctx.show(&retitled.workspace, |workspace| {
        outln!(
            "Set the title of {} to {}",
            workspace.name,
            workspace.title.as_deref().unwrap_or_default()
        )
    })?;
    Ok(0)
}

/// Show the retitled workspace's label on the places Shoal created for it,
/// where they still show the label its previous title gave them.
async fn relabel(ctx: &Context, retitled: &RetitledWorkspace) -> Result<()> {
    let RetitledWorkspace {
        workspace,
        previous_title,
    } = retitled;
    let places =
        crate::client::workspaces::places(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    let places: Vec<_> = places.into_iter().map(|owned| owned.origin).collect();
    let target = ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
    let labels = terminal::Retitled::new(ctx, workspace, previous_title.as_deref());
    terminal::relabel_space(&settings, &places, async |host| labels.labels(host).await).await;
    Ok(())
}

/// Ask the workspace's running agent to choose its title, through the
/// messages `shoal messages send` queues. Without an agent the message
/// would wait unread until removal, so that is refused.
pub(in crate::cli::commands) async fn request_title(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<i32> {
    let selector = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let (workspace, agents) = turns::agents_of(ctx, selector).await?;
    ensure!(
        !agents.is_empty(),
        "{name} has no running agent to ask; set its title with shoal work rename {name} <TITLE>",
        name = workspace.name
    );
    let message = TITLE_REQUEST.into();
    crate::client::agent_messages::send(
        &ctx.paths,
        SpaceSelector::id(&workspace.id),
        message,
        Vec::new(),
        None,
    )
    .await?;
    ctx.emit("Asked the workspace's agent to choose its title.", ())?;
    Ok(0)
}
