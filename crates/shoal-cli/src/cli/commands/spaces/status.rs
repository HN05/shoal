//! A space's status: what the daemon reports and the sessions the CLI finds,
//! or another joined machine's status of its space, or what that machine
//! last reported while it is offline.
mod render;
mod reported;

use anyhow::{Context as _, Result, bail, ensure};
use shoal_protocol::status::{SpaceDetail, SpaceStatus, StatusAnswer};

use self::render::render_status;
use crate::client::{output::status::StatusView, transcripts};
use crate::{
    cli::{commands::service, context::Context, ui},
    forge::IssueInput,
    model::SpaceTarget,
    protocol::ConfigTarget,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn status(
    ctx: &Context,
    space: Option<String>,
    item: Option<String>,
) -> Result<i32> {
    let Some(target) = status_target(space.as_deref(), item)? else {
        let space = space.as_deref().map(SpaceSelector::parse).transpose()?;
        let space = match space {
            // A named space needs no picker.
            Some(space) => {
                ui::select_space(ctx, Some(space), ui::Fallback::CurrentDirectoryOnly).await?
            }
            None => match space_here(ctx).await? {
                Some(space) => space,
                None => return service::shoal_status(ctx).await,
            },
        };
        match crate::client::spaces::status(&ctx.paths, space).await? {
            StatusAnswer::Live(status) => {
                let status = status_view(ctx, *status).await;
                ctx.show(&status, |status| render_status(status, ctx.json))?;
            }
            StatusAnswer::Reported(reported) => reported::show(ctx, &reported)?,
        }
        return Ok(0);
    };
    let spaces = crate::client::spaces::find_spaces(&ctx.paths, target).await?;
    let mut statuses = Vec::new();
    for space in spaces {
        let selector = SpaceSelector::id(&space.id);
        // The spaces it finds are this machine's, whose status is its own.
        let StatusAnswer::Live(status) =
            crate::client::spaces::status(&ctx.paths, selector).await?
        else {
            bail!("space {} has no status of its own here", space.id);
        };
        statuses.push(status_view(ctx, *status).await);
    }
    ctx.show(&statuses, |statuses| {
        for (index, status) in statuses.iter().enumerate() {
            if index > 0 {
                outln!();
            }
            render_status(status, ctx.json);
        }
    })?;
    Ok(0)
}

/// The space this runs in; none outside every space, or without a daemon,
/// which could not say which workspace the current directory is in. A
/// daemon whose request slots are full refuses that lookup but still
/// answers its status, which tells why, so a failed lookup is none too.
async fn space_here(ctx: &Context) -> Result<Option<SpaceSelector>> {
    if crate::cli::client::status(&ctx.paths).await?.is_none() {
        return Ok(None);
    }
    match ui::space_here(ctx, ui::Workspaces::Any).await {
        Ok(space) => Ok(space),
        Err(error) => {
            eprintln!("warning: cannot tell which space this runs in: {error:#}");
            Ok(None)
        }
    }
}

/// `status` with the sessions its terminal hosts keep and where its
/// agents' transcripts are, which another machine that runs the space
/// keeps there.
async fn status_view(ctx: &Context, status: SpaceStatus) -> StatusView {
    if status.machine.is_some() {
        return StatusView {
            status,
            sessions: Vec::new(),
            sessions_error: None,
            transcripts: Vec::new(),
            transcripts_error: None,
        };
    }
    let (sessions, sessions_error) = listed(sessions(ctx, &status.space).await);
    let id = status.space.id().to_owned();
    let (transcripts, transcripts_error) =
        listed(transcripts::space_transcripts(&ctx.paths, SpaceSelector::id(&id)).await);
    StatusView {
        status,
        sessions: sessions
            .into_iter()
            .map(crate::terminal::Session::output)
            .collect(),
        sessions_error,
        transcripts,
        transcripts_error,
    }
}

/// What `found` lists, or why it could not, which status shows in place of
/// the list.
fn listed<T>(found: Result<Vec<T>>) -> (Vec<T>, Option<String>) {
    match found {
        Ok(found) => (found, None),
        Err(error) => (Vec::new(), Some(format!("{error:#}"))),
    }
}

/// The agent sessions of `space`, in the hosts its settings enable and at
/// the places the daemon recorded it running in.
async fn sessions(ctx: &Context, space: &SpaceDetail) -> Result<Vec<crate::terminal::Session>> {
    let target = match space {
        SpaceDetail::Workspace(status) => {
            ConfigTarget::Space(SpaceSelector::id(&status.workspace.id))
        }
        SpaceDetail::Chat { chat } => ConfigTarget::chat(chat),
    };
    let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
    let places = crate::client::places::places(&ctx.paths, SpaceSelector::id(space.id()));
    let places = places.await?;
    crate::terminal::sessions(
        &settings,
        &crate::terminal::Recorded::space(&places.executions, &places.places),
    )
    .await
}

/// Linked items find the spaces that link them, and resources the
/// workspaces that hold them; anything else selects a space.
fn status_target(first: Option<&str>, item: Option<String>) -> Result<Option<SpaceTarget>> {
    match first {
        Some("resource") => Ok(Some(SpaceTarget::Resource {
            name: item.context("resource needs a pool or member name")?,
        })),
        Some(first)
            if matches!(first, "pr" | "issue" | "ci")
                || IssueInput::parse(first) == IssueInput::Url =>
        {
            let items = crate::cli::args::items::ItemArgs {
                kind_or_url: Some(first.parse()?),
                item,
                space: None,
            };
            let selection = items.selection()?;
            Ok(Some(SpaceTarget::Item {
                kind: selection.kind.context("expected an issue, PR or CI run")?,
                input: selection
                    .input
                    .with_context(|| format!("{first} needs a number or URL"))?,
            }))
        }
        _ => {
            ensure!(
                item.is_none(),
                "put pr, issue, ci or resource before a number, URL or name"
            );
            Ok(None)
        }
    }
}
