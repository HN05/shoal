//! The `SubagentStart` hook. A subagent inherits its agent's scope, and under
//! Codex its instructions and directory too, so without a brief it would act
//! on its agent's workspace, finishing that assignment as the instructions
//! say. The hook tells it which workspace it works in instead.
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::Deserialize;

use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    model::Workspace,
};

/// What `SubagentStart` passes besides the common fields.
#[derive(Deserialize)]
struct Start {
    /// Where the subagent starts: a child's worktree Claude Code created for
    /// it, or its agent's directory.
    cwd: PathBuf,
}

/// Print the subagent's brief as hook context. Failing leaves the subagent
/// without it and never stops it from starting.
pub(super) async fn start(ctx: &Context) -> Result<i32> {
    let input = hook_input();
    match brief(ctx, &input).await {
        Ok(brief) => outln!("{}", super::messages::hook_output(&input, &brief)),
        Err(error) => eprintln!("shoal: subagent brief unavailable: {error:#}"),
    }
    Ok(0)
}

/// The hook's input on stdin; unreadable input reads as none.
fn hook_input() -> String {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    input
}

/// The brief for a subagent starting as `input` describes.
async fn brief(ctx: &Context, input: &str) -> Result<String> {
    let start: Start = serde_json::from_str(input).context("read the hook's input")?;
    let workspaces = client::workspaces(&ctx.paths).await?;
    let own = own_workspace(ctx, &workspaces).await?;
    let child = super::worktree_hooks::enclosing(&workspaces, &start.cwd)
        .ok()
        .filter(|child| child.id != own.id);
    Ok(text(own, child))
}

/// The workspace the agent's scope belongs to.
async fn own_workspace<'a>(ctx: &Context, workspaces: &'a [Workspace]) -> Result<&'a Workspace> {
    let own = ui::select_workspace(ctx, None, Fallback::OwnWorkspace).await?;
    workspaces
        .iter()
        .find(|workspace| workspace.id == own)
        .context("the agent's workspace is not listed")
}

/// A subagent started in `child` works there; one started in `own` works in
/// the child its agent's brief names.
fn text(own: &Workspace, child: Option<&Workspace>) -> String {
    let own = &own.name;
    match child {
        Some(child) => format!(
            "You are a subagent working in Shoal workspace {}, a child of {own}, at {}. \
             Shoal commands you run there act on {0}. Leave {own} to the agent that \
             started you, which finishes it.",
            child.name,
            child.path.display()
        ),
        None => format!(
            "You are a subagent of the agent working in Shoal workspace {own}, which \
             finishes {own} itself: never run `shoal done`, `shoal ready`, \
             `shoal pr open` or another command that finishes or marks work in {own}. \
             Work in the child workspace your brief names and run Shoal commands from \
             its directory."
        ),
    }
}
