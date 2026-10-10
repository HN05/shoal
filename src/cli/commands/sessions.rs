//! Persistent agent sessions run in zmx; attaching hands this terminal to one.
use anyhow::{Result, bail, ensure};

use crate::cli::{
    client,
    context::Context,
    ui::{self, Fallback},
    zmx,
};

pub(super) async fn attach(
    ctx: &Context,
    workspace: Option<String>,
    session: Option<String>,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "attach to sessions outside scoped executions"
    );
    ensure!(ctx.interactive(), "attach requires a terminal");
    let id = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, id).await?.workspace;
    let sessions = zmx::sessions(&workspace.id).await?;
    let name = match (session, sessions.as_slice()) {
        (Some(name), _) => {
            ensure!(
                sessions.iter().any(|session| session.name == name),
                "{} has no session {name}",
                workspace.name
            );
            name
        }
        (None, []) => bail!("{} has no agent sessions", workspace.name),
        (None, [only]) => only.name.clone(),
        (None, several) => {
            let labels: Vec<_> = several.iter().map(zmx::Session::label).collect();
            let choices: Vec<_> = several
                .iter()
                .zip(&labels)
                .map(|(session, label)| (session.name.clone(), label.as_str()))
                .collect();
            ui::pick_choice(ctx, "Attach session> ", &choices)?
        }
    };
    zmx::attach(&workspace, &name).await
}
