//! Renaming a workspace, which the places Shoal opened for it show, or
//! asking its agent to.
use anyhow::{Result, ensure};

use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    cli::{
        commands::turns,
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    model::RenamedWorkspace,
    protocol::ConfigTarget,
};

/// What `shoal rename --request` asks the workspace's agent.
const NAME_REQUEST: &str = "The user asks you to rename this workspace: name the work as it stands now in a few words with `shoal rename \"<name>\"`.";

pub(in crate::cli::commands) async fn set_name(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: SpaceName,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let renamed = crate::client::workspaces::set_name(&ctx.paths, workspace, name).await?;
    // The name is recorded; a place left with its old label only warns.
    if let Err(error) = relabel(ctx, &renamed).await {
        eprintln!(
            "warning: cannot relabel the places of {}: {error:#}",
            renamed.workspace.label()
        );
    }
    ctx.show(&renamed.workspace, |workspace| {
        outln!(
            "Renamed workspace {} {} to {}",
            workspace.number,
            renamed.previous_name,
            workspace.name
        )
    })?;
    Ok(0)
}

/// Show the renamed workspace's label on the places Shoal created for it,
/// where they still show the label its previous name gave them.
async fn relabel(ctx: &Context, renamed: &RenamedWorkspace) -> Result<()> {
    let RenamedWorkspace {
        workspace,
        previous_name,
    } = renamed;
    let places =
        crate::client::workspaces::places(&ctx.paths, SpaceSelector::id(&workspace.id)).await?;
    let places: Vec<_> = places.into_iter().map(|owned| owned.origin).collect();
    let target = ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
    let labels = terminal::Renamed::new(ctx, workspace, previous_name);
    terminal::relabel_space(&settings, &places, async |host| labels.labels(host).await).await;
    Ok(())
}

/// Ask the workspace's running agent to choose its name, through the
/// messages `shoal messages send` queues. Without an agent the message
/// would wait unread until removal, so that is refused.
pub(in crate::cli::commands) async fn request_name(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let selector = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let (workspace, agents) = turns::agents_of(ctx, selector).await?;
    ensure!(
        !agents.is_empty(),
        "{} has no running agent to ask; set its name with shoal rename {} <NAME>",
        workspace.label(),
        workspace.number
    );
    crate::client::agent_messages::send(
        &ctx.paths,
        SpaceSelector::id(&workspace.id),
        NAME_REQUEST.into(),
        Vec::new(),
        None,
    )
    .await?;
    ctx.emit("Asked the workspace's agent to choose its name.", ())?;
    Ok(0)
}
