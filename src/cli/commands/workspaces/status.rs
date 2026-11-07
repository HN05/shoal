//! A space's status: what the daemon reports and the sessions the CLI finds.
mod render;

use anyhow::{Context as _, Result, ensure};

use self::render::render_status;
use crate::client::space_names::SpaceSelector;
use crate::client::{output::status::StatusView, transcripts};
use crate::{
    cli::{
        context::Context,
        terminal::{self, Recorded},
        ui,
    },
    forge::{IssueInput, items::link::Selection},
    model::{SpaceDetail, SpaceTarget},
    protocol::ConfigTarget,
};

pub(in crate::cli::commands) async fn status(
    ctx: &Context,
    space: Option<String>,
    item: Option<String>,
) -> Result<i32> {
    let Some(target) = status_target(space.as_deref(), item)? else {
        let space = ui::select_space_here(ctx, space).await?;
        let status = status_view(ctx, space).await?;
        ctx.show(&status, |status| render_status(status, ctx.json))?;
        return Ok(0);
    };
    let spaces = crate::client::workspaces::find_spaces(&ctx.paths, target).await?;
    let mut statuses = Vec::new();
    for space in spaces {
        statuses.push(status_view(ctx, space.id().to_owned()).await?);
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

/// The status of `space`, with the sessions its terminal hosts keep and
/// where its agents' transcripts are.
async fn status_view(ctx: &Context, space: String) -> Result<StatusView> {
    let status =
        crate::client::workspaces::status(&ctx.paths, SpaceSelector::parse(&space)).await?;
    let (sessions, sessions_error) = listed(sessions(ctx, &status.space).await);
    let id = status.space.id().to_owned();
    let (transcripts, transcripts_error) =
        listed(transcripts::space_transcripts(&ctx.paths, SpaceSelector::id(&id)).await);
    Ok(StatusView {
        status,
        sessions: sessions
            .into_iter()
            .map(crate::terminal::Session::output)
            .collect(),
        sessions_error,
        transcripts,
        transcripts_error,
    })
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
    let (target, places) = match space {
        SpaceDetail::Workspace(status) => (
            ConfigTarget::Workspace(SpaceSelector::id(&status.workspace.id)),
            Recorded::workspace(&status.executions, &status.places),
        ),
        SpaceDetail::Chat { chat } => (ConfigTarget::chat(chat), Recorded::chat(chat)),
    };
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
    terminal::sessions(&settings, &places).await
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
            let selection = Selection::parse(Some(first.to_owned()), item)?;
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
