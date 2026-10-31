//! `shoal review run`: queue a review of a workspace's changes or a PR, or
//! run the configured `review` command.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    agent::Agent,
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    config::Effective,
    forge::{
        ForgeRepo, IssueInput, PullRequest,
        items::link::{ItemKind, Link, LinkTarget},
    },
    git,
    model::{Repository, Workspace},
    protocol::{ConfigTarget, ReviewMethod, WorkspaceMethod},
    reviews::{Review, ReviewTarget},
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
            let repo = super::super::issues::item_repository(
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
            fetch_pull_branch(&repo.path, "base", &pull.base).await?;
            update_to_head(ctx, &repo.path, &workspace, &pull.head).await?;
            let workspace =
                super::super::workspaces::link_pull(ctx, workspace, pull.url.clone()).await?;
            eprintln!(
                "Reviewing PR #{} in workspace {}",
                pull.number, workspace.name
            );
            workspace.id
        }
        None => {
            let tracking = fetch_pull_branch(&repo.path, "base", &pull.base).await?;
            let creation = super::super::workspaces::Creation {
                existing: Some(git::remote_ref("origin", &pull.head)),
                base: Some(tracking),
                pr: Some(pull.url.clone()),
                ..Default::default()
            };
            let code = super::super::workspaces::add(
                ctx,
                Some(repo.id.clone()),
                creation,
                None,
                super::super::workspaces::AgentLaunch::Explicit(None),
                Vec::new(),
                super::super::workspaces::AddOptions {
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
pub(in crate::cli::commands) async fn fetch_pull_branch(
    path: &Path,
    role: &str,
    branch: &str,
) -> Result<String> {
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
    ensure!(
        workspace.is_some() || crate::env::chat().is_none(),
        "a chat has no changes to review; name a workspace"
    );
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
            let agent =
                configured_reviewer(&settings).map_or_else(|| "pick an agent".into(), String::from);
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
            ensure!(
                args.is_empty(),
                "arguments after -- go to the configured review command; pass --manual"
            );
            let target = subject.target(post.unwrap_or(settings.review.post));
            queue(ctx, workspace, &settings, agent, target).await
        }
        _ => super::super::configured::run(ctx, MANUAL, Some(workspace), args).await,
    }
}

/// The agent that reviews without --agent: `[review] agent`, else
/// `agent.default`.
fn configured_reviewer(settings: &Effective) -> Option<Agent> {
    settings
        .review
        .agent
        .clone()
        .or_else(|| settings.agent.default.clone())
}

/// Queue a review by `agent`, else the configured reviewer, else one picked.
async fn queue(
    ctx: &Context,
    workspace: String,
    settings: &Effective,
    agent: Option<Agent>,
    target: ReviewTarget,
) -> Result<i32> {
    let agent = match agent.or_else(|| configured_reviewer(settings)) {
        Some(agent) => agent,
        None => match crate::cli::agents::pick_default_agent(ctx, settings)? {
            Some(agent) => agent,
            None => return Ok(0),
        },
    };
    // Refused here, before the daemon queues a review it could not run.
    crate::cli::agents::ensure_available(agent.name(), settings)?;
    let review: Review = client::reviews(
        &ctx.paths,
        ReviewMethod::RunReview {
            workspace,
            target,
            agent: Some(agent),
        },
    )
    .await?;
    let message = format!(
        "Review {id} of {} is queued for {}; its findings arrive as a message. Wait for it with \
         shoal watch review {id}.",
        review.workspace,
        review.agent,
        id = review.id,
    );
    ctx.emit(&message, &review)?;
    Ok(0)
}

impl Subject {
    /// What the daemon reviews, with whether a PR review posts.
    fn target(self, post: bool) -> ReviewTarget {
        match self {
            Self::Changes => ReviewTarget::Workspace,
            Self::Pull { pull, .. } => ReviewTarget::Pr {
                url: pull.url,
                head: pull.head,
                base: pull.base,
                post,
            },
        }
    }
}

#[cfg(test)]
mod tests;
