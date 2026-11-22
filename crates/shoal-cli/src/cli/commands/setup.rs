//! `shoal setup`: run a workspace's setup again or skip a failed one, and
//! the CLI's side of every workspace's preparation: its spinner, and asking
//! the user on a terminal what to do with a failed setup.
use anyhow::{Result, ensure};
use shoal_workers::{
    following::Display,
    preparation::{self, Frontend, Settling},
};

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Acting, Fallback},
    },
    model::Workspace,
    protocol::operations::{OperationSpec, Preparation},
    state::WorkspaceState,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn setup(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = ui::select_workspace(
        ctx,
        workspace,
        Fallback::CurrentDirectory(&preparing(Preparation {
            setup: true,
            post_setup: true,
            ..Default::default()
        })),
    )
    .await?;
    let inspection = crate::client::workspaces::inspect(workspace.clone())
        .send(&ctx.paths)
        .await?;
    let set_up = preparation::setup_workspace(&ctx.paths, ctx, &inspection.workspace, true).await?;
    let Some(set_up) = set_up else {
        return Ok(1);
    };
    let workspace = set_up.finish(&ctx.paths, ctx).await?;
    ctx.emit_styled(
        Style::Success,
        &format!("Set up {}", workspace.name),
        &workspace,
    )?;
    Ok(0)
}

/// Accept a failed setup as it is: mark the workspace ready once its
/// worktree verifies and no setup process remains.
pub(in crate::cli::commands) async fn skip_setup(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = ui::select_workspace(
        ctx,
        workspace,
        Fallback::CurrentDirectory(&preparing(Preparation {
            post_setup: true,
            ..Default::default()
        })),
    )
    .await?;
    let workspace = crate::client::workspaces::inspect(workspace.clone())
        .send(&ctx.paths)
        .await?
        .workspace;
    ensure!(
        workspace.state == WorkspaceState::Failed,
        "{} is {}; only a failed setup can be skipped",
        workspace.name,
        workspace.state
    );
    preparation::ignore_failure(&ctx.paths, &workspace).await?;
    let workspace = crate::client::workspaces::inspect(SpaceSelector::id(&workspace.id))
        .send(&ctx.paths)
        .await?
        .workspace;
    preparation::run_post_setup(&ctx.paths, ctx, &workspace).await?;
    ctx.emit_styled(
        Style::Success,
        &format!("Skipped setup of {}", workspace.name),
        &workspace,
    )?;
    Ok(0)
}

/// What setup sends for the workspace it picks: an operation that takes
/// `steps` in it on this machine, where its worktree is.
fn preparing(steps: Preparation) -> Acting {
    Acting::request(move |workspace| {
        let spec = OperationSpec::Prepare {
            workspace,
            steps: steps.clone(),
        };
        crate::client::operations::start_request(spec, Default::default())
    })
}

/// A command shows an operation's steps under its spinner, and asks a user
/// on its terminal what to do with a failed setup.
impl Frontend for Context {
    fn json(&self) -> bool {
        self.json
    }

    fn display(&self) -> impl Display {
        super::operations::steps(self)
    }

    fn settle(&self, workspace: &Workspace, error: &str, ran: bool) -> Result<Option<Settling>> {
        if !self.interactive() {
            return Ok(None);
        }
        let failed = match ran {
            true => "Setup failed",
            false => "Setup did not start",
        };
        eprintln!(
            "{} for {}: {error}",
            Palette::stderr(self.json).paint(Style::Error, failed),
            workspace.name
        );
        Ok(Some(match ui::setup_failure_choice(ran)? {
            Settling::Delete if !deletes(self, workspace)? => Settling::Cancel,
            settling => settling,
        }))
    }

    fn unverified(&self, message: String) -> Result<()> {
        ensure!(self.interactive(), "{message}");
        eprintln!(
            "{}",
            Palette::stderr(self.json).paint(Style::Warning, message)
        );
        Ok(())
    }
}

/// Whether the user confirms deleting `workspace` with its setup's changes.
fn deletes(ctx: &Context, workspace: &Workspace) -> Result<bool> {
    ui::confirm(
        ctx,
        &format!(
            "Delete workspace {} at {} (including setup changes)?",
            workspace.name,
            workspace.path.display(),
        ),
        "--yes",
    )
}
