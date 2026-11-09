//! `shoal review run`: queue a review of a workspace's changes or a PR, or
//! run the configured `review` command.
use crate::client::space_ids::SpaceId;
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail, ensure};

use crate::client::operations::NewWorkspace;
use crate::client::space_names::SpaceSelector;
use crate::{
    agent::{Agent, ModelChoice},
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    config::Effective,
    forge::{
        IssueInput, PullRequest,
        items::link::{ItemKind, Link, LinkTarget},
    },
    git,
    model::{Chat, Repository, Workspace},
    protocol::{ConfigTarget, CreationOptions, ItemContext, operations::WorkspaceStart},
    reviews::ReviewTarget,
};

/// The configured command a manual review runs.
const MANUAL: &str = "review";

/// Why an agent review refuses arguments after `--`.
const FORWARDED_ARGS: &str =
    "arguments after -- go to the configured review command; pass --manual";

#[derive(Clone)]
pub(super) enum Reviewer {
    Ask,
    Manual,
    Agent(Option<Agent>),
}

/// What `shoal review` reviews.
pub(super) enum Target {
    /// A workspace's changes, or a chat's linked PR, which `post` applies to.
    Space {
        space: Option<SpaceSelector>,
        post: Option<bool>,
    },
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
        workspace: Option<SpaceSelector>,
        pr: Option<String>,
        repository: Option<String>,
        post: Option<bool>,
    ) -> Result<Self> {
        let url = workspace.as_ref().map(ToString::to_string);
        let url = url.filter(|input| IssueInput::parse(input) == IssueInput::Url);
        let input = match (pr, url) {
            (Some(input), _) => input,
            (None, Some(input)) => match Link::parse(&input)?.target {
                LinkTarget::Pr => input,
                LinkTarget::Issue => {
                    bail!(
                        "shoal review run takes a workspace or a PR; settle an issue's plan with shoal start --mode plan --issue {input}"
                    )
                }
                LinkTarget::Run | LinkTarget::Branch(_) => bail!("expected a PR URL"),
            },
            (None, None) => {
                ensure!(
                    repository.is_none(),
                    "--repo selects the repository of a PR; pass --pr or a URL"
                );
                return Ok(Target::Space {
                    space: workspace,
                    post,
                });
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

/// Review the target with the chosen reviewer, an agent with `model`, or
/// forward `args` to the configured review command.
pub(super) async fn start(
    ctx: &Context,
    target: Target,
    reviewer: Reviewer,
    model: ModelChoice,
    args: Vec<OsString>,
) -> Result<i32> {
    // Refused before a workspace is picked, opened or updated.
    ensure!(
        args.is_empty() || !reviewer.chosen_agent(&target),
        "{FORWARDED_ARGS}"
    );
    match target {
        Target::Space { space, post } => match super::select(ctx, space).await? {
            // A chat has no changes of its own, so it reviews its linked PR.
            super::Space::Chat(chat) => {
                let (repo, url) = chat_pull(ctx, &chat).await?;
                check_reviewer(ctx, &repo, &reviewer, post, &model).await?;
                let pull =
                    read_pull(ctx, ItemContext::Chat(SpaceSelector::id(&chat.id)), url).await?;
                pull_request(ctx, &repo, pull, reviewer, post, model, args).await
            }
            super::Space::Workspace(workspace) => {
                ensure!(
                    post.is_none(),
                    "--post and --no-post apply to PR reviews; workspace reviews stay local"
                );
                let subject = Subject::Changes;
                run(ctx, Some(workspace), reviewer, subject, model, args).await
            }
        },
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
                "shoal review run <url> --repo <repository>",
            )
            .await?;
            check_reviewer(ctx, &repo, &reviewer, post, &model).await?;
            let context = super::super::issues::read_context(ctx, &repo).await?;
            let pull = read_pull(ctx, context, input).await?;
            pull_request(ctx, &repo, pull, reviewer, post, model, args).await
        }
    }
}

/// The chat's one linked PR, with the chat's repository.
async fn chat_pull(ctx: &Context, chat: &Chat) -> Result<(Repository, String)> {
    let url = match chat.links.prs.as_slice() {
        [url] => url.clone(),
        [] => bail!("{} links no PR; pass --pr or a PR URL", chat.name),
        _ => bail!("{} links several PRs; pass --pr or a PR URL", chat.name),
    };
    let repo = (crate::client::repositories::list(&ctx.paths)
        .await?
        .into_iter())
    .find(|repo| Some(&repo.id) == chat.repository_id.as_ref())
    .with_context(|| format!("{}'s repository is not registered", chat.name))?;
    Ok((repo, url))
}

/// Refuse an agent reviewer that could not review before a PR's workspace
/// is opened or updated: the named agent, else the configured reviewer,
/// without its review command or the templates `model` needs. Choosing
/// whether to post, or having no manual review command, chooses an agent.
async fn check_reviewer(
    ctx: &Context,
    repo: &Repository,
    reviewer: &Reviewer,
    post: Option<bool>,
    model: &ModelChoice,
) -> Result<()> {
    let settings = super::super::chat::settings(ctx, repo).await?;
    let agent = match reviewer {
        Reviewer::Agent(agent) => agent.clone(),
        Reviewer::Ask if post.is_some() || !settings.commands.contains_key(MANUAL) => None,
        _ => return Ok(()),
    };
    let Some(agent) = agent.or_else(|| configured_reviewer(&settings)) else {
        return Ok(());
    };
    super::super::chat::check_model(&settings, &agent, model)?;
    crate::cli::agents::ensure_reviewer_available(&agent, &settings)
}

impl Reviewer {
    /// Whether the options alone choose an agent reviewer for `target`:
    /// `--agent`, `--model` or `--effort`, or choosing whether to post.
    fn chosen_agent(&self, target: &Target) -> bool {
        match self {
            Reviewer::Agent(_) => true,
            Reviewer::Ask => matches!(
                target,
                Target::Pull { post: Some(_), .. } | Target::Space { post: Some(_), .. }
            ),
            Reviewer::Manual => false,
        }
    }

    /// `--manual`, else `--agent`; a requested `model` chooses an agent
    /// reviewer, since only an agent runs with one.
    pub fn new(manual: bool, agent: Option<Agent>, model: &ModelChoice) -> Self {
        match (manual, agent) {
            (true, _) => Reviewer::Manual,
            (false, Some(agent)) => Reviewer::Agent(Some(agent)),
            (false, None) if !model.is_empty() => Reviewer::Agent(None),
            (false, None) => Reviewer::Ask,
        }
    }
}

/// The PR `input` names in the repository of `context`, before a
/// workspace is opened or updated for it.
async fn read_pull(ctx: &Context, context: ItemContext, input: String) -> Result<PullRequest> {
    let read = client::pull_request(&ctx.paths, context, input);
    ctx.progress("Reading PR", read).await
}

/// Review `pull` in the workspace that owns its head branch, opening one
/// from origin that is compared against the PR's base when none does.
async fn pull_request(
    ctx: &Context,
    repo: &Repository,
    pull: PullRequest,
    reviewer: Reviewer,
    post: Option<bool>,
    model: ModelChoice,
    args: Vec<OsString>,
) -> Result<i32> {
    if pull.fork {
        let workspace = fork_pull_workspace(ctx, repo, &pull).await?;
        let subject = Subject::Pull { pull, post };
        return run(
            ctx,
            Some(SpaceSelector::Id(workspace)),
            reviewer,
            subject,
            model,
            args,
        )
        .await;
    }
    let owner = crate::client::workspaces::list(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| {
            workspace.repository_id == repo.id && workspace.branch.as_ref() == Some(&pull.head)
        });
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
            let creation = crate::start_plan::Creation {
                existing: Some(git::remote_ref("origin", &pull.head)),
                base: Some(tracking),
                pr: Some(pull.url.clone()),
                ..Default::default()
            };
            let code = super::super::workspaces::add(
                ctx,
                Some(repo.id.clone().into()),
                creation,
                None,
                super::super::workspaces::AgentLaunch::Explicit(None),
                Vec::new(),
                super::super::workspaces::AddOptions {
                    here: true,
                    focus: false,
                    now: false,
                    after: Vec::new(),
                    ..Default::default()
                },
            )
            .await?;
            if code != 0 {
                return Ok(code);
            }
            crate::client::workspaces::list(&ctx.paths)
                .await?
                .into_iter()
                .find(|workspace| {
                    workspace.repository_id == repo.id
                        && workspace.branch.as_ref() == Some(&pull.head)
                })
                .context("the opened PR workspace is missing")?
                .id
        }
    };
    let subject = Subject::Pull { pull, post };
    run(
        ctx,
        Some(SpaceSelector::id(&workspace)),
        reviewer,
        subject,
        model,
        args,
    )
    .await
}

/// The workspace at a commit a fork PR's review reads: the one at its head,
/// or a new one without setup, since the reviewer only reads a checkout,
/// with the PR linked.
async fn fork_pull_workspace(
    ctx: &Context,
    repo: &Repository,
    pull: &PullRequest,
) -> Result<SpaceId> {
    let start = WorkspaceStart::PullHead {
        url: pull.url.clone(),
        head: super::super::links::fork_head(repo, pull).await?,
    };
    let new = NewWorkspace {
        parent: crate::cli::ui::current_chat(&None)
            .as_ref()
            .map(SpaceSelector::id),
        options: CreationOptions {
            skip_setup: true,
            ..Default::default()
        },
        pr: Some(pull.url.clone()),
        ..NewWorkspace::new((&repo.id).into(), start)
    };
    let env = crate::env::without_secrets();
    let started = crate::client::operations::start_workspace(&ctx.paths, new, env).await?;
    // Reopening runs no hooks; only a new workspace is prepared.
    let Some(workspace) = super::super::split::follow_preparation(ctx, started).await? else {
        bail!("PR #{}'s workspace could not be set up", pull.number);
    };
    eprintln!(
        "Reviewing PR #{} in workspace {}",
        pull.number, workspace.name
    );
    Ok(workspace.id)
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
    let updated = crate::client::workspaces::fast_forward(
        &ctx.paths,
        SpaceSelector::id(&workspace.id),
        head.to_owned(),
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
    workspace: Option<SpaceSelector>,
    reviewer: Reviewer,
    subject: Subject,
    model: ModelChoice,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings =
        crate::client::workspaces::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone()))
            .await?;
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
            ensure!(args.is_empty(), "{FORWARDED_ARGS}");
            let target = subject.target(post.unwrap_or(settings.review.post));
            queue(ctx, workspace, &settings, agent, model, target).await
        }
        _ => {
            if matches!(subject, Subject::Pull { .. }) {
                ensure_at_pull_head(ctx, &workspace).await?;
            }
            super::super::configured::run(ctx, MANUAL, Some(workspace.clone()), args).await
        }
    }
}

/// Refuse a manual PR review in a workspace at a commit whose HEAD moved past
/// the PR head it was opened at: the review command reads the checkout, so
/// it would review commits the PR does not have.
async fn ensure_at_pull_head(ctx: &Context, workspace: &SpaceSelector) -> Result<()> {
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace.clone())
        .await?
        .workspace;
    let Some(at) = &workspace.at else {
        return Ok(());
    };
    let head = git::revision(&workspace.path, "HEAD", git::run).await?;
    ensure!(
        head.trim() == at.commit,
        "workspace {} has commits of its own on top of the PR's head, which a manual review would include; review with an agent, or remove the workspace to open the PR again",
        workspace.name
    );
    Ok(())
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

/// Queue a review by `agent`, else the configured reviewer, else one picked,
/// with `model`.
async fn queue(
    ctx: &Context,
    workspace: SpaceSelector,
    settings: &Effective,
    agent: Option<Agent>,
    model: ModelChoice,
    target: ReviewTarget,
) -> Result<i32> {
    let agent = match agent.or_else(|| configured_reviewer(settings)) {
        Some(agent) => agent,
        None => match crate::cli::agents::pick_reviewer(ctx, settings)? {
            Some(agent) => agent,
            None => return Ok(0),
        },
    };
    // Refused here, before the daemon queues a review it could not run.
    super::super::chat::check_model(settings, &agent, &model)?;
    crate::cli::agents::ensure_reviewer_available(&agent, settings)?;
    let review =
        crate::client::reviews::run(&ctx.paths, workspace.clone(), target, agent, model).await?;
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
