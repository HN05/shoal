//! Persistent agent sessions run in terminal hosts; attaching hands this
//! terminal to one.
use anyhow::{Context as _, Result, bail, ensure};

use crate::cli::{
    client,
    context::Context,
    terminal,
    ui::{self, Fallback},
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
    let sessions = terminal::sessions(&workspace.id).await?;
    let session = match (session, sessions.as_slice()) {
        (Some(name), _) => sessions
            .iter()
            .find(|session| session.name == name)
            .with_context(|| format!("{} has no session {name}", workspace.name))?,
        (None, []) => bail!("{} has no agent sessions", workspace.name),
        (None, [only]) => only,
        (None, several) => {
            let labels: Vec<_> = several.iter().map(terminal::Session::label).collect();
            let choices: Vec<_> = several
                .iter()
                .zip(&labels)
                .map(|(session, label)| (session, label.as_str()))
                .collect();
            ui::pick_choice(ctx, "Attach session> ", &choices)?
        }
    };
    terminal::attach(&workspace, session).await
}
