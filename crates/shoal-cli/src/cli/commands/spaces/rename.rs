//! `shoal rename`: rename a workspace or a chat, a picked one with what the
//! user types, or ask its agent to choose.
use anyhow::{Result, ensure};

use crate::{
    cli::{
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    model::{RenamedSpace, SpacePlaces},
    protocol::ConfigTarget,
    sessions::Origin,
};
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::status::StatusAnswer;

/// What `shoal rename --request` asks a space's agent.
const NAME_REQUEST: &str = "The user asks you to rename this space: name the work as it stands now in a few words with `shoal rename \"<name>\"`.";

/// Rename the space `space` selects, else the space this runs in or a
/// picked one.
pub(in crate::cli::commands) async fn rename(
    ctx: &Context,
    space: Option<SpaceSelector>,
    name: String,
) -> Result<i32> {
    // Refuse a name before a picker asks for the space.
    let name = SpaceName::parse(&name)?;
    let space = chosen_space(ctx, space).await?;
    rename_space(ctx, space, name).await
}

/// Rename a picked workspace or chat with what the user types next.
pub(in crate::cli::commands) async fn rename_picked(ctx: &Context) -> Result<i32> {
    let space = ui::pick_any_space(ctx).await?;
    let name = ask_name(ctx).await?;
    rename_space(ctx, space, name).await
}

/// Give the space `space` selects the name `name`, and show it on the
/// places that still show its previous one.
async fn rename_space(ctx: &Context, space: SpaceSelector, name: SpaceName) -> Result<i32> {
    let renamed = crate::client::workspaces::rename_space(&ctx.paths, space, name).await?;
    // The name is recorded; a place left with its old label only warns.
    if let Err(error) = relabel(ctx, &renamed).await {
        eprintln!(
            "warning: cannot relabel the places of {}: {error:#}",
            renamed.space.label()
        );
    }
    ctx.show(&renamed.space, |space| {
        outln!(
            "Renamed space {} {} to {}",
            space.number,
            renamed.previous_name,
            &space.name
        )
    })?;
    Ok(0)
}

/// Show the renamed space's label on the places its agents run in and those
/// Shoal created for it, where they still show the label its previous name
/// gave them.
async fn relabel(ctx: &Context, renamed: &RenamedSpace) -> Result<()> {
    let RenamedSpace {
        space,
        previous_name,
    } = renamed;
    let places =
        crate::client::workspaces::places(&ctx.paths, SpaceSelector::id(&space.id)).await?;
    let shown = shown_places(places);
    let settings = shoal_workers::settings::of(&ctx.paths, ConfigTarget::space(space)).await?;
    let labels = terminal::Renamed::new(ctx, space, previous_name);
    terminal::relabel_space(&settings, &shown, async |host| labels.labels(host).await).await;
    Ok(())
}

/// The places that show a space's label: those its agents run in and
/// those Shoal created for it, each once.
fn shown_places(places: SpacePlaces) -> Vec<Origin> {
    let agents = (places.executions.into_iter()).filter(|execution| execution.agent.is_some());
    let running = agents.filter_map(|execution| execution.origin);
    let mut shown = Vec::new();
    for place in running.chain(places.places) {
        if !shown.contains(&place) {
            shown.push(place);
        }
    }
    shown
}

/// Ask for a space's new name until it is one a space can have.
async fn ask_name(ctx: &Context) -> Result<SpaceName> {
    ui::input_until(
        ctx,
        "Name",
        None,
        |name| async move { SpaceName::parse(&name) },
    )
    .await
}

/// Ask the running agent of the space `space` selects, else the one this
/// runs in or a picked one, to choose its name, through the alert
/// `shoal send` queues. Without an agent the alert would wait
/// unread, so that is refused.
pub(in crate::cli::commands) async fn request_name(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    super::super::start::ensure_scope_token(ctx)?;
    let selector = chosen_space(ctx, space).await?;
    let asked = asked_space(ctx, &selector).await?;
    ensure!(
        asked.running,
        "{} has no running agent to ask; set its name with shoal rename {} <NAME>",
        asked.label,
        asked.number
    );
    let to = shoal_protocol::protocol::AlertTarget::Space {
        space: asked.selector,
    };
    let request = NAME_REQUEST.into();
    crate::client::alerts::send(&ctx.paths, to, None, request, Vec::new()).await?;
    ctx.emit("Asked the space's agent to choose its name.", ())?;
    Ok(0)
}

/// The space `rename --request` asks, by its ID, with whether an agent runs
/// or sleeps there to ask.
struct AskedSpace {
    selector: SpaceSelector,
    label: String,
    number: SpaceNumber,
    running: bool,
}

/// The space `selector` selects, on whichever machine runs it, as
/// `rename --request` asks it: as its machine reports it, or while that
/// machine is offline, as it last reported it.
async fn asked_space(ctx: &Context, selector: &SpaceSelector) -> Result<AskedSpace> {
    let status = crate::client::workspaces::status(&ctx.paths, selector.clone()).await?;
    let space = match status {
        StatusAnswer::Live(status) => status.space,
        StatusAnswer::Reported(reported) => {
            let space = reported.space.space;
            return Ok(AskedSpace {
                selector: SpaceSelector::id(&space.id),
                label: shoal_protocol::space_names::label(space.number, &space.name),
                number: space.number,
                running: space.agent_runs_or_sleeps(),
            });
        }
    };
    let id = SpaceSelector::id(space.id());
    let agents = crate::client::agents::running(&ctx.paths, id.clone()).await?;
    Ok(AskedSpace {
        selector: id,
        label: space.label(),
        number: space.number(),
        running: !agents.is_empty(),
    })
}

/// The space `space` names, else the current workspace or the chat this runs
/// in, else the current checkout's only running chat, else the current
/// workspace or a picked space.
async fn chosen_space(ctx: &Context, space: Option<SpaceSelector>) -> Result<SpaceSelector> {
    if space.is_none() && ui::current_chat(&None).is_none() {
        let chats = crate::client::spaces::list(&ctx.paths)
            .await?
            .into_running_chats();
        if let Some(chat) = super::super::sessions::checkout_chat(ctx, &chats).await? {
            return Ok(SpaceSelector::Id(chat.id));
        }
    }
    ui::select_space(ctx, space, Fallback::CurrentDirectory).await
}
