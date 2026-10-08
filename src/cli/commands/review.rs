//! Review a workspace's changes with the configured `review` command or an agent.
use std::ffi::OsString;

use anyhow::{Context as _, Result, bail};

use crate::{
    agent::{Agent, CodexMode},
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    forge::{ForgeRepo, PullRequest},
    git,
    model::Workspace,
    protocol::ConfigTarget,
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
    let mut repos = client::repositories(&ctx.paths).await?;
    let repository = match repository {
        Some(repository) => {
            let selector = ui::repository_selector(repository)?;
            match super::repositories::offer_unregistered(ctx, &selector).await? {
                Some(repo) => {
                    let id = repo.id.clone();
                    repos.push(repo);
                    id.into()
                }
                None => selector,
            }
        }
        None if input.starts_with("https://") || input.starts_with("http://") => {
            super::issues::registered_remote(
                ctx,
                &mut repos,
                ForgeRepo::from_pull_url(&input)?,
                "shoal pr review <url> --repo <repository>",
            )
            .await?
            .into()
        }
        None => super::issues::current_repository(ctx, repos.clone(), super::issues::REPO_OR_URL)
            .await?
            .into(),
    };
    let repo = crate::forge::repository::select(&repos, &repository).await?;
    let remote = crate::forge::repository::remote_url_from_path(&repo.path)
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
            let tracking = fetch_pull_base(&repo.path, &pull.base).await?;
            let creation = super::workspaces::Creation {
                path: None,
                branch: None,
                existing: Some(git::remote_ref("origin", &pull.head)),
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
                true,
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

pub(super) async fn fetch_pull_base(path: &std::path::Path, base: &str) -> Result<String> {
    git::check_branch_name(None, base)
        .await
        .with_context(|| format!("PR base {base:?} is not a branch name"))?;
    let tracking = git::remote_ref("origin", base);
    git::run(
        path,
        &[
            git::FETCH_SAFE_ARGS,
            &[
                "--refmap=",
                "--",
                "origin",
                &format!("+{}:{tracking}", git::local_ref(base)),
            ],
        ]
        .concat(),
    )
    .await
    .with_context(|| format!("could not fetch the PR base {base}"))?;
    Ok(tracking)
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
            let Some(agent) = crate::cli::agents::default_agent(
                ctx,
                ConfigTarget::Workspace(workspace.clone()),
                agent,
            )
            .await?
            else {
                return Ok(0);
            };
            let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
            let prompt = prompt(&workspace, pull.as_ref());
            // Desktop apps cannot receive the prompt.
            crate::cli::agents::launch_agent(
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
