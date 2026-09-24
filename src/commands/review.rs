//! Review a workspace's changes with the configured `review` command or an agent.
use std::ffi::OsString;

use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{Agent, CodexMode},
    client,
    context::Context,
    forge::{ForgeRepo, PullRequest},
    git,
    model::Workspace,
    protocol::ConfigTarget,
    ui::{self, Fallback},
};

/// The configured command a manual review runs.
const MANUAL: &str = "review";

#[derive(Clone)]
pub(super) enum Reviewer {
    Ask,
    Manual,
    Agent(Option<Agent>),
}

impl Reviewer {
    pub fn new(manual: bool, agent: Option<Agent>) -> Self {
        match (manual, agent) {
            (true, _) => Reviewer::Manual,
            (false, Some(agent)) => Reviewer::Agent(Some(agent)),
            (false, None) => Reviewer::Ask,
        }
    }
}

/// Review a PR in the workspace that owns its head branch, opening one from
/// origin that is compared against the PR's base when none does.
pub(super) async fn pull_request(
    ctx: &Context,
    input: String,
    repository: Option<String>,
    reviewer: Reviewer,
    args: Vec<OsString>,
) -> Result<i32> {
    let repos = client::repositories(&ctx.paths).await?;
    let repository = match repository {
        Some(repository) => ui::repository_selector(repository)?,
        None if input.starts_with("https://") || input.starts_with("http://") => {
            super::issues::repository_with_remote(
                &repos,
                ForgeRepo::from_pull_url(&input)?,
                "shoal pr review <url> --repo <repository>",
            )
            .await?
            .id
            .clone()
        }
        None => super::issues::repository_for_number(ctx, repos.clone()).await?,
    };
    let repo = crate::repository::select(&repos, &repository).await?;
    let remote =
        crate::repository::remote_url(repo.path.to_str().context("repository path is not UTF-8")?)
            .await?
            .context("PR review needs an origin remote")?;
    let pull = ForgeRepo::parse(&remote)?
        .pull_request(&repo.path, &input)
        .await?;
    let owner = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.repository_id == repo.id && workspace.branch == pull.head);
    let workspace = match owner {
        Some(workspace) => {
            eprintln!(
                "Reviewing PR #{} in workspace {}",
                pull.number, workspace.name
            );
            workspace.id
        }
        None => {
            git::check_branch_name(None, &pull.base)
                .await
                .with_context(|| format!("PR base {:?} is not a branch name", pull.base))?;
            // The base is compared through its remote-tracking ref, so bring it
            // up to date without touching local branches.
            let tracking = format!("refs/remotes/origin/{}", pull.base);
            git::run(
                &repo.path,
                &[
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--refmap=",
                    "--",
                    "origin",
                    &format!("+refs/heads/{}:{tracking}", pull.base),
                ],
            )
            .await
            .with_context(|| format!("could not fetch the PR base {}", pull.base))?;
            let creation = super::workspaces::Creation {
                path: None,
                branch: None,
                existing: Some(format!("refs/remotes/origin/{}", pull.head)),
                base: Some(tracking),
                git_profile: None,
            };
            let code = super::workspaces::add(
                ctx,
                Some(repo.id.clone()),
                creation,
                None,
                super::workspaces::AgentLaunch::Explicit(None),
                Vec::new(),
            )
            .await?;
            if code != 0 {
                return Ok(code);
            }
            client::workspaces(&ctx.paths)
                .await?
                .into_iter()
                .find(|workspace| {
                    workspace.repository_id == repo.id && workspace.branch == pull.head
                })
                .context("the opened PR workspace is missing")?
                .id
        }
    };
    run(ctx, Some(workspace), reviewer, Some(pull), args).await
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
            ui::pick_choice(
                ctx,
                "Review> ",
                &[
                    (Reviewer::Manual, &format!("Manual ({MANUAL} command)")),
                    (Reviewer::Agent(None), &format!("Agent ({agent})")),
                ],
            )?
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
        _ => crate::config::named_commands::run(ctx, MANUAL, Some(workspace), args).await,
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
