//! Following the setup of a workspace a start recorded, then reporting and
//! entering it.
use anyhow::Result;

use crate::client::preparation::{self, Preparing, Unprepared};
use crate::{
    agent::Agent,
    cli::{
        agents,
        commands::{
            operations,
            workspaces::{SetUp, setup::settle_setup},
        },
        context::Context,
        output::{Palette, Style},
        shell, terminal,
    },
    client::connection::CommandPaths,
    model::Workspace,
    protocol::{
        ConfigTarget,
        operations::{Preparation, Prepared},
    },
};
use shoal_protocol::space_names::SpaceSelector;

/// Follow the preparation of the started workspace, which sets it up, links
/// its PR and queues its agent, before reporting it, unless the start
/// `named` it, which reports only its agent, and entering it unless `agent`
/// takes the terminal.
pub(super) async fn finish_add_workspace(
    ctx: &Context,
    preparing: Preparing<'_, CommandPaths>,
    agent: Option<&Agent>,
    named: bool,
) -> Result<Option<Prepared>> {
    let reused = preparing.reused();
    let Some(SetUp {
        workspace,
        post_setup,
        agent: entry,
    }) = follow_add_setup(ctx, preparing).await?
    else {
        return Ok(None);
    };
    // The post-setup hook may change how the agent launches, so the shell's
    // destination waits for it; a failed hook starts no agent.
    let (prepared, post_setup) = match post_setup {
        Some(result) => (
            Prepared {
                workspace: Box::new(workspace),
                agent: entry,
            },
            result,
        ),
        None => prepare_after_setup(ctx, workspace, reused).await?,
    };
    let workspace = &prepared.workspace;
    if !named {
        ctx.show(workspace, |workspace| {
            outln!(
                "{} {} {}",
                if reused { "Opened" } else { "Created" },
                Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
                super::super::location_phrase(workspace),
            );
        })?;
    }
    let starts = agent.filter(|_| post_setup.is_ok());
    enter_workspace(ctx, workspace, starts).await?;
    post_setup?;
    Ok(Some(prepared))
}

/// Follow the preparation of the started workspace and settle a failed
/// setup, which leaves its agent waiting in the queue, as its preparation
/// queued it; a retry runs what follows setup with it.
async fn follow_add_setup(
    ctx: &Context,
    preparing: Preparing<'_, CommandPaths>,
) -> Result<Option<SetUp>> {
    let retry = Preparation {
        setup: true,
        post_setup: !preparing.reused(),
        ..Default::default()
    };
    let prepared = operations::follow_preparation(ctx, preparing).await?;
    settle_setup(ctx, prepared, Some(retry)).await
}

/// Run what preparation leaves after setup once the user ignored its
/// failure: the post-setup hook unless the workspace was `reused`.
/// Returns the workspace with its agent, and how the hook went.
async fn prepare_after_setup(
    ctx: &Context,
    workspace: Workspace,
    reused: bool,
) -> Result<(Prepared, Result<()>)> {
    let steps = Preparation {
        post_setup: !reused,
        ..Default::default()
    };
    let env = crate::env::without_secrets();
    let preparing = preparation::prepare(&ctx.paths, &workspace, steps, env).await?;
    let ended = operations::follow_preparation(ctx, preparing).await?;
    Ok(match ended {
        Ok(prepared) => (prepared, Ok(())),
        Err(Unprepared { workspace, failure }) => {
            let agent = None;
            let error = crate::operations::error(failure);
            (Prepared { workspace, agent }, Err(error))
        }
    })
}

/// Have the caller's shell enter `workspace` once `agent`, if any, ends,
/// where [`enters`] says it does.
pub(in crate::cli::commands) async fn enter_workspace(
    ctx: &Context,
    workspace: &Workspace,
    agent: Option<&Agent>,
) -> Result<()> {
    if enters(ctx, workspace, agent).await? {
        shell::navigate(ctx, &workspace.path)?;
    }
    Ok(())
}

/// Whether the caller's shell enters `workspace`, where `agent` starts
/// unless `None`. An agent that runs in a session this terminal attaches
/// to leaves it where it was, as detaching returns to that shell rather
/// than to the workspace.
async fn enters(ctx: &Context, workspace: &Workspace, agent: Option<&Agent>) -> Result<bool> {
    if !terminal::follows_caller(ctx) {
        return Ok(false);
    }
    let Some(chosen) = agent else {
        return Ok(true);
    };
    let settings = shoal_workers::settings::of(
        &ctx.paths,
        ConfigTarget::Space(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    Ok(!agents::runs_in_session(chosen, &settings))
}
