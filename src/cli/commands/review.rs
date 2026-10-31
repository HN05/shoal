//! Review a workspace's changes or a PR with the configured `review` command or an agent.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    agent::Agent,
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    forge::{
        ForgeRepo, IssueInput, PullRequest,
        items::link::{ItemKind, Link, LinkTarget},
    },
    git,
    model::{Repository, Workspace},
    protocol::{ConfigTarget, WorkspaceMethod},
};

/// The configured command a manual review runs.
const MANUAL: &str = "review";

#[derive(Clone)]
pub(super) enum Reviewer {
    Ask,
    Manual,
    Agent(Option<Agent>),
}

/// What `shoal review` reviews.
pub(super) enum Target {
    Workspace(Option<String>),
    Pull {
        input: String,
        repository: Option<String>,
        /// `--post` or `--no-post`; otherwise `[review] post` decides.
        post: Option<bool>,
    },
}

impl Target {
    /// A URL in the workspace position names a PR; `--repo` and posting
    /// apply only to PRs.
    pub fn new(
        workspace: Option<String>,
        pr: Option<String>,
        repository: Option<String>,
        post: Option<bool>,
    ) -> Result<Self> {
        let input = match (pr, workspace) {
            (Some(input), _) => input,
            (None, Some(input)) if IssueInput::parse(&input) == IssueInput::Url => {
                match Link::parse(&input)?.target {
                    LinkTarget::Pr => input,
                    LinkTarget::Issue => {
                        bail!(
                            "shoal review takes a workspace or a PR; settle an issue's plan with shoal plan {input}"
                        )
                    }
                    LinkTarget::Run | LinkTarget::Branch(_) => bail!("expected a PR URL"),
                }
            }
            (None, workspace) => {
                ensure!(
                    repository.is_none(),
                    "--repo selects the repository of a PR; pass --pr or a URL"
                );
                ensure!(
                    post.is_none(),
                    "--post and --no-post apply to PR reviews; workspace reviews stay local"
                );
                return Ok(Target::Workspace(workspace));
            }
        };
        Ok(Target::Pull {
            input,
            repository,
            post,
        })
    }
}

/// The PR an agent review covers, if any, and whether it posts there.
enum Subject {
    Changes,
    Pull {
        pull: PullRequest,
        post: Option<bool>,
    },
}

/// Review the target with the chosen reviewer, forwarding `args` to it.
pub(super) async fn start(
    ctx: &Context,
    target: Target,
    reviewer: Reviewer,
    args: Vec<OsString>,
) -> Result<i32> {
    match target {
        Target::Workspace(workspace) => run(ctx, workspace, reviewer, Subject::Changes, args).await,
        Target::Pull {
            input,
            repository,
            post,
        } => {
            let repo = super::issues::item_repository(
                ctx,
                ItemKind::Pr,
                Some(&input),
                repository,
                "shoal review <url> --repo <repository>",
            )
            .await?;
            pull_request(ctx, &repo, &input, reviewer, post, args).await
        }
    }
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
async fn pull_request(
    ctx: &Context,
    repo: &Repository,
    input: &str,
    reviewer: Reviewer,
    post: Option<bool>,
    args: Vec<OsString>,
) -> Result<i32> {
    let remote = crate::repository::remote_url_from_path(&repo.path)
        .await?
        .context("PR review needs an origin remote")?;
    let pull = ForgeRepo::resolve(&remote, &ctx.forge_hosts()?)
        .await?
        .pull_request(&ctx.forge_account()?, input)
        .await?;
    let owner = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.repository_id == repo.id && workspace.branch == pull.head);
    let workspace = match owner {
        Some(workspace) => {
            update_to_head(ctx, &repo.path, &workspace, &pull.head).await?;
            let workspace = super::workspaces::link_pull(ctx, workspace, pull.url.clone()).await?;
            eprintln!(
                "Reviewing PR #{} in workspace {}",
                pull.number, workspace.name
            );
            workspace.id
        }
        None => {
            let tracking = fetch_pull_branch(&repo.path, "base", &pull.base).await?;
            let creation = super::workspaces::Creation {
                existing: Some(git::remote_ref("origin", &pull.head)),
                base: Some(tracking),
                pr: Some(pull.url.clone()),
                ..Default::default()
            };
            let code = super::workspaces::add(
                ctx,
                Some(repo.id.clone()),
                creation,
                None,
                super::workspaces::AgentLaunch::Explicit(None),
                Vec::new(),
                super::workspaces::AddOptions {
                    here: true,
                    focus: false,
                    now: false,
                    after: Vec::new(),
                },
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
    let subject = Subject::Pull { pull, post };
    run(ctx, Some(workspace), reviewer, subject, args).await
}

/// Fast-forward a reused workspace to the PR's pushed head so the review sees
/// the author's latest commits; local commits ahead of it stay as they are.
async fn update_to_head(
    ctx: &Context,
    repo: &Path,
    workspace: &Workspace,
    head: &str,
) -> Result<()> {
    let tracking = fetch_pull_branch(repo, "head", head).await?;
    let updated: bool = client::request(
        &ctx.paths,
        WorkspaceMethod::FastForwardWorkspace {
            workspace: workspace.id.clone(),
            branch: head.to_owned(),
        },
    )
    .await?;
    if updated {
        eprintln!("Updated workspace {} to {tracking}", workspace.name);
    }
    Ok(())
}

/// Fetch the PR's `role` (head or base) branch into its origin tracking ref.
pub(super) async fn fetch_pull_branch(path: &Path, role: &str, branch: &str) -> Result<String> {
    git::check_branch_name(None, branch)
        .await
        .with_context(|| format!("PR {role} {branch:?} is not a branch name"))?;
    git::fetch::review_branch(path, branch)
        .await
        .with_context(|| format!("could not fetch the PR {role} {branch}"))
}

async fn run(
    ctx: &Context,
    workspace: Option<String>,
    reviewer: Reviewer,
    subject: Subject,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    let post = match &subject {
        Subject::Changes => None,
        Subject::Pull { post, .. } => *post,
    };
    let reviewer = match reviewer {
        // Only an agent posts, so choosing whether to post chooses the agent.
        Reviewer::Ask if post.is_some() => Reviewer::Agent(None),
        Reviewer::Ask if !settings.commands.contains_key(MANUAL) => Reviewer::Agent(None),
        Reviewer::Ask if ctx.interactive() => {
            let agent = settings
                .agent
                .default
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
            let post = post.unwrap_or(settings.review.post);
            let prompt = prompt(&workspace, &subject, post);
            crate::cli::agents::launch_agent(
                ctx,
                agent,
                workspace,
                Some(prompt),
                args,
                crate::cli::agents::LaunchOptions::default(),
            )
            .await
        }
        _ => super::configured::run(ctx, MANUAL, Some(workspace), args).await,
    }
}

fn prompt(workspace: &Workspace, subject: &Subject, post: bool) -> String {
    let (subject, delivery) = match subject {
        Subject::Pull { pull, .. } => (
            format!(
                "Review pull request #{}: {}\n{}\n\nIts changes are on branch {}",
                pull.number, pull.title, pull.url, workspace.branch
            ),
            if post {
                format!(
                    "Post them as one comment on pull request #{} without approving or \
                     requesting changes. Do not edit files, commit, or push.",
                    pull.number
                )
            } else {
                LOCAL.to_owned()
            },
        ),
        Subject::Changes => (
            format!("Review the changes on branch {}", workspace.branch),
            LOCAL.to_owned(),
        ),
    };
    format!(
        "{subject}; `shoal diff` shows them against the base it forked from.\n\n\
         Report findings ordered by severity, each with a file and line and the input \
         or state that triggers it. {delivery}"
    )
}

/// How a review that does not post delivers its findings.
const LOCAL: &str = "Do not edit files, commit, push, or comment on the forge unless asked.";

#[cfg(test)]
mod tests;
