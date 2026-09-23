//! Review a workspace's changes with the configured `review` command or an agent.
use std::ffi::OsString;

use anyhow::{Result, bail};

use crate::{
    cli::{Agent, CodexMode},
    client,
    context::Context,
    model::Workspace,
    protocol::ConfigTarget,
    ui::{self, Fallback},
};

/// The configured command a manual review runs.
const MANUAL: &str = "review";

pub(super) enum Reviewer {
    Ask,
    Manual,
    Agent(Option<Agent>),
}

/// The pull request under review, named in an agent's prompt.
pub(super) struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
}

pub(super) async fn run(
    ctx: &Context,
    workspace: Option<String>,
    reviewer: Reviewer,
    pull: Option<PullRequest>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    let reviewer = match reviewer {
        Reviewer::Ask if !settings.commands.contains_key(MANUAL) => Reviewer::Agent(None),
        Reviewer::Ask if ctx.interactive() => {
            let agent = settings
                .default_agent
                .clone()
                .map_or_else(|| "pick an agent".into(), String::from);
            let choice = ui::pick(
                ctx,
                "Review> ",
                vec![
                    ("manual".into(), format!("Manual ({MANUAL} command)")),
                    ("agent".into(), format!("Agent ({agent})")),
                ],
            )?;
            if choice == "manual" {
                Reviewer::Manual
            } else {
                Reviewer::Agent(None)
            }
        }
        Reviewer::Ask => bail!("choose a reviewer with --manual or --agent <name>"),
        reviewer => reviewer,
    };
    match reviewer {
        Reviewer::Agent(agent) => {
            let agent = super::workspaces::default_agent(
                ctx,
                ConfigTarget::Workspace(workspace.clone()),
                agent,
            )
            .await?;
            let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
            let prompt = prompt(&workspace, pull.as_ref());
            // Desktop apps cannot receive the prompt.
            super::workspaces::launch_agent(
                ctx,
                agent,
                Some(CodexMode::Cli),
                workspace,
                Some(prompt),
                args,
            )
            .await
        }
        _ => crate::named_commands::run(ctx, MANUAL, Some(workspace), args).await,
    }
}

fn prompt(workspace: &Workspace, pull: Option<&PullRequest>) -> String {
    let subject = match pull {
        Some(pull) => format!(
            "Review pull request #{}: {}\n{}\n\nIts changes are on branch {}",
            pull.number, pull.title, pull.url, workspace.branch
        ),
        None => format!("Review the changes on branch {}", workspace.branch),
    };
    format!(
        "{subject}; `shoal diff` shows them against the base it forked from.\n\n\
         Report findings ordered by severity, each with a file and line and the input \
         or state that triggers it. Do not edit files, commit, push, or comment on the \
         forge unless asked."
    )
}
