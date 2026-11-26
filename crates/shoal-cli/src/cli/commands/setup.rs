//! `shoal setup`, and the CLI's side of every workspace's preparation.
use anyhow::{Result, ensure};
use shoal_workers::{
    following::Display,
    preparation::{self, FailedSetup, Frontend, Settling},
};

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Acting, Fallback},
    },
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
        Fallback::CurrentDirectory(&setting_up(false)),
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
        Fallback::CurrentDirectory(&setting_up(true)),
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

/// With `skip`, only the post-setup hook runs, which accepts a failed setup.
pub(super) fn setting_up(skip: bool) -> Acting {
    preparing(Preparation {
        setup: !skip,
        post_setup: true,
        ..Default::default()
    })
}

fn preparing(steps: Preparation) -> Acting {
    Acting::request(move |workspace| {
        let spec = OperationSpec::Prepare {
            workspace,
            steps: steps.clone(),
        };
        crate::client::operations::start_request(spec, Default::default())
    })
}

impl Frontend for Context {
    fn json(&self) -> bool {
        self.json
    }

    fn display(&self) -> impl Display {
        super::operations::steps(self)
    }

    fn settle(&self, failed: &FailedSetup<'_>) -> Result<Option<Settling>> {
        if !self.interactive() {
            return Ok(None);
        }
        let heading = match failed.ran {
            true => "Setup failed",
            false => "Setup did not start",
        };
        eprintln!(
            "{} for {}: {}",
            Palette::stderr(self.json).paint(Style::Error, heading),
            failed.workspace.name,
            failed.error
        );
        Ok(Some(match ui::setup_failure_choice(failed.ran)? {
            Settling::Delete if !deletes(self, failed)? => Settling::Cancel,
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

/// Whether the user confirms deleting the workspace whose setup `failed`
/// with its setup's changes, naming the machine its worktree is on when
/// another runs it.
fn deletes(ctx: &Context, failed: &FailedSetup<'_>) -> Result<bool> {
    let workspace = failed.workspace;
    let on = (failed.machine).map_or_else(String::new, |machine| format!(" on {}", machine.name));
    ui::confirm(
        ctx,
        &format!(
            "Delete workspace {} at {}{on} (including setup changes)?",
            workspace.name,
            workspace.path.display(),
        ),
        "--yes",
    )
}
