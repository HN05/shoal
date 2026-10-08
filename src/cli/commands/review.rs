//! Review a workspace's changes with the configured `review` command or an agent.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    agent::{Agent, CodexMode},
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    forge::{ForgeRepo, IssueInput, PullRequest},
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

/// What `shoal review` reviews.
pub(super) enum Target {
    Workspace(Option<String>),
    Pull {
        input: String,
        repository: Option<String>,
    },
}

impl Target {
    /// A URL in the workspace position names a PR; `--repo` selects a PR's repository.
    pub fn new(
        workspace: Option<String>,
        pr: Option<String>,
        repository: Option<String>,
    ) -> Result<Self> {
        let pasted = workspace
            .as_deref()
            .is_some_and(|input| IssueInput::parse(input) == IssueInput::Url);
        match (pr, pasted) {
            (Some(input), _) => Ok(Target::Pull { input, repository }),
            (None, true) => Ok(Target::Pull {
                input: workspace.unwrap(),
                repository,
            }),
            (None, false) => {
                ensure!(
                    repository.is_none(),
                    "--repo selects a PR's repository; pass --pr or a PR URL"
                );
                Ok(Target::Workspace(workspace))
            }
        }
    }
}

/// Review the target with the chosen reviewer, forwarding `args` to it.
pub(super) async fn start(
    ctx: &Context,
    target: Target,
    reviewer: Reviewer,
    args: Vec<OsString>,
) -> Result<i32> {
    match target {
        Target::Workspace(workspace) => run(ctx, workspace, reviewer, None, args).await,
        Target::Pull { input, repository } => {
            pull_request(ctx, input, repository, reviewer, args).await
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
                "shoal review <url> --repo <repository>",
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
            update_to_head(&repo.path, &workspace, &pull.head).await?;
            eprintln!(
                "Reviewing PR #{} in workspace {}",
                pull.number, workspace.name
            );
            workspace.id
        }
        None => {
            let tracking = fetch_pull_branch(&repo.path, "base", &pull.base).await?;
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

/// Fast-forward a reused workspace to the PR's pushed head so the review sees
/// the author's latest commits; local commits ahead of it stay as they are.
async fn update_to_head(repo: &Path, workspace: &Workspace, head: &str) -> Result<()> {
    let tracking = fetch_pull_branch(repo, "head", head).await?;
    let path = &workspace.path;
    if git::is_ancestor(path, &tracking, "HEAD", git::isolated_command).await? {
        return Ok(());
    }
    ensure!(
        git::is_ancestor(path, "HEAD", &tracking, git::isolated_command).await?,
        "workspace {} has diverged from {tracking}; update it before reviewing",
        workspace.name
    );
    git::run_without_submodules(
        path,
        &[
            "merge",
            "--ff-only",
            "--no-edit",
            "--no-stat",
            "--no-overwrite-ignore",
            &tracking,
        ],
    )
    .await
    .with_context(|| {
        format!(
            "could not fast-forward workspace {} to {tracking}",
            workspace.name
        )
    })?;
    eprintln!("Updated workspace {} to {tracking}", workspace.name);
    Ok(())
}

/// Fetch the PR's `role` (head or base) branch into its origin tracking ref.
pub(super) async fn fetch_pull_branch(path: &Path, role: &str, branch: &str) -> Result<String> {
    git::check_branch_name(None, branch)
        .await
        .with_context(|| format!("PR {role} {branch:?} is not a branch name"))?;
    let tracking = git::remote_ref("origin", branch);
    git::run(
        path,
        &[
            git::FETCH_SAFE_ARGS,
            &[
                "--refmap=",
                "--",
                "origin",
                &format!("+{}:{tracking}", git::local_ref(branch)),
            ],
        ]
        .concat(),
    )
    .await
    .with_context(|| format!("could not fetch the PR {role} {branch}"))?;
    Ok(tracking)
}

async fn run(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_pr_flags_select_a_pr_and_repo_requires_one() {
        let url = "https://forge.example/team/repo/pulls/7";
        for (workspace, pr) in [(Some(url), None), (None, Some("7"))] {
            let target = Target::new(
                workspace.map(String::from),
                pr.map(String::from),
                Some("repo".into()),
            )
            .unwrap();
            assert!(matches!(
                target,
                Target::Pull {
                    repository: Some(_),
                    ..
                }
            ));
        }
        assert!(matches!(
            Target::new(Some("fix-login".into()), None, None).unwrap(),
            Target::Workspace(Some(name)) if name == "fix-login"
        ));
        assert!(Target::new(Some("fix-login".into()), None, Some("repo".into())).is_err());
    }
}
