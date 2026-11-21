//! The subagent hooks. A subagent inherits its agent's scope, and under
//! Codex its instructions and directory too, so without a brief it would act
//! on its agent's workspace, finishing that assignment as the instructions
//! say: the `SubagentStart` hook tells it which workspace it works in
//! instead. Every agent's hooks also report its subagents' starts and
//! stops, which keep it from sleep while it idles.
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::Deserialize;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    agents::integration, ai::IntegrationKind, client::connection::CommandPaths, model::Workspace,
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
pub async fn start(paths: &CommandPaths) -> Result<i32> {
    let input = hook_input();
    match brief(paths, &input).await {
        Ok(brief) => shoal_core::outln!("{}", super::output(&input, &brief)),
        Err(error) => eprintln!("shoal: subagent brief unavailable: {error:#}"),
    }
    Ok(0)
}

/// Report the change to the running subagents that the hook event on stdin
/// of an agent with `integration` names, for the space `space` names, else
/// the agent's own. An event that changes none reports nothing.
pub async fn report(
    paths: &CommandPaths,
    space: Option<SpaceSelector>,
    integration: IntegrationKind,
) -> Result<i32> {
    let event = super::input::event();
    let change = event.and_then(|event| self::integration(integration).subagent_change(&event));
    let Some(change) = change else {
        return Ok(0);
    };
    let space = super::own_space(paths, space).await?;
    crate::client::agents::report_subagent(paths, space, change).await?;
    Ok(0)
}

/// The hook's input on stdin; unreadable input reads as none.
fn hook_input() -> String {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    input
}

/// The brief for a subagent starting as `input` describes.
async fn brief(paths: &CommandPaths, input: &str) -> Result<String> {
    let start: Start = serde_json::from_str(input).context("read the hook's input")?;
    let workspaces = crate::client::spaces::list(paths).await?.into_workspaces();
    let own = listed_own(paths, &workspaces).await?;
    let child = super::enclosing(&workspaces, &start.cwd)
        .ok()
        .filter(|child| child.id != own.id);
    Ok(text(own, child))
}

/// The workspace the agent's scope belongs to, among `workspaces`.
async fn listed_own<'a>(
    paths: &CommandPaths,
    workspaces: &'a [Workspace],
) -> Result<&'a Workspace> {
    let own = super::own_workspace(paths).await?;
    workspaces
        .iter()
        .find(|workspace| own.selects(&workspace.id, workspace.number))
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
