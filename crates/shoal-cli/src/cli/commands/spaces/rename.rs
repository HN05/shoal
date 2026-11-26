//! `shoal rename`.
use anyhow::{Result, ensure};
use shoal_workers::hosting;

use crate::{
    cli::{
        context::Context,
        ui::{self, Acting, Fallback},
    },
    model::{RenamedSpace, SpacePlaces},
    protocol::ConfigTarget,
    sessions::Origin,
};
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::status::StatusAnswer;

const NAME_REQUEST: &str = "The user asks you to rename this space: name the work as it stands now in a few words with `shoal rename \"<name>\"`.";

pub(in crate::cli::commands) async fn rename(
    ctx: &Context,
    space: Option<SpaceSelector>,
    name: String,
) -> Result<i32> {
    // Refuse a name before a picker asks for the space.
    let name = SpaceName::parse(&name)?;
    let space = chosen_space(ctx, space, &renaming()).await?;
    rename_space(ctx, space, name).await
}

pub(in crate::cli::commands) async fn rename_picked(ctx: &Context) -> Result<i32> {
    let space = ui::pick_any_space(ctx, &renaming()).await?;
    let name = ask_name(ctx).await?;
    rename_space(ctx, space, name).await
}

/// The name, which may be typed once the space is picked, decides nothing of
/// which spaces the rename reaches.
pub(in crate::cli::commands) fn renaming() -> Acting {
    let name = SpaceName::from(String::from("name"));
    Acting::request(move |space| crate::client::spaces::rename_space(space, name.clone()))
}

pub(in crate::cli::commands) fn asking() -> Acting {
    Acting::request(|space| {
        let to = shoal_protocol::protocol::AlertTarget::Space { space };
        crate::client::alerts::send(to, None, NAME_REQUEST.into(), Vec::new())
    })
}

async fn rename_space(ctx: &Context, space: SpaceSelector, name: SpaceName) -> Result<i32> {
    let renamed = (crate::client::spaces::rename_space(space, name))
        .send(&ctx.paths)
        .await?;
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

async fn relabel(ctx: &Context, renamed: &RenamedSpace) -> Result<()> {
    let RenamedSpace {
        space,
        previous_name,
    } = renamed;
    let places = crate::client::places::places(&ctx.paths, SpaceSelector::id(&space.id)).await?;
    let shown = shown_places(places);
    let settings = shoal_workers::settings::of(&ctx.paths, ConfigTarget::space(space)).await?;
    let labels = hosting::Renamed::new(&ctx.paths, space, previous_name);
    hosting::relabel_space(&settings, &shown, async |host| labels.labels(host).await).await;
    Ok(())
}

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

async fn ask_name(ctx: &Context) -> Result<SpaceName> {
    ui::input_until(
        ctx,
        "Name",
        None,
        |name| async move { SpaceName::parse(&name) },
    )
    .await
}

/// Without an agent the alert would wait unread, so that is refused.
pub(in crate::cli::commands) async fn request_name(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    super::super::start::ensure_scope_token(ctx)?;
    let selector = chosen_space(ctx, space, &asking()).await?;
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
    (crate::client::alerts::send(to, None, request, Vec::new()))
        .send(&ctx.paths)
        .await?;
    ctx.emit("Asked the space's agent to choose its name.", ())?;
    Ok(0)
}

struct AskedSpace {
    selector: SpaceSelector,
    label: String,
    number: SpaceNumber,
    running: bool,
}

/// While its machine is offline, as that machine last reported it.
async fn asked_space(ctx: &Context, selector: &SpaceSelector) -> Result<AskedSpace> {
    let status = crate::client::spaces::status(selector.clone())
        .send(&ctx.paths)
        .await?;
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

async fn chosen_space(
    ctx: &Context,
    space: Option<SpaceSelector>,
    acting: &Acting,
) -> Result<SpaceSelector> {
    if space.is_none() && ui::current_chat(&None).is_none() {
        let chats = crate::client::spaces::list(&ctx.paths)
            .await?
            .into_running_chats();
        if let Some(chat) = super::super::sessions::checkout_chat(ctx, &chats).await? {
            return Ok(SpaceSelector::Id(chat.id));
        }
    }
    ui::select_space(ctx, space, Fallback::CurrentDirectory(acting)).await
}
