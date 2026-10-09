//! Review a workspace's changes with the configured `review` command or an agent.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    agent::{Agent, CodexMode},
    cli::{
        client,
        commands::issues::Issue,
        context::Context,
        ui::{self, Fallback},
    },
    forge::{
        ForgeRepo, IssueInput, PullRequest,
        link::{ItemKind, Link, LinkTarget},
    },
    git,
    model::{Repository, Workspace},
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
    Item {
        kind: ItemKind,
        input: String,
        repository: Option<String>,
        /// `--post` or `--no-post`; otherwise `[review] post` decides.
        post: Option<bool>,
    },
}

impl Target {
    /// A URL in the workspace position names a PR or issue; `--repo` and
    /// posting apply only to those forge items.
    pub fn new(
        workspace: Option<String>,
        item: Option<(ItemKind, String)>,
        repository: Option<String>,
        post: Option<bool>,
    ) -> Result<Self> {
        let item = match (item, workspace) {
            (Some(item), _) => Some(item),
            (None, Some(input)) if IssueInput::parse(&input) == IssueInput::Url => {
                let kind = match Link::parse(&input)?.target {
                    LinkTarget::Pr => ItemKind::Pr,
                    LinkTarget::Issue => ItemKind::Issue,
                    LinkTarget::Branch(_) => bail!("expected a PR or issue URL"),
                };
                Some((kind, input))
            }
            (None, workspace) => {
                ensure!(
                    repository.is_none(),
                    "--repo selects the repository of a PR or issue; pass --pr, --issue or a URL"
                );
                ensure!(
                    post.is_none(),
                    "--post and --no-post apply to PR and issue reviews; workspace reviews stay local"
                );
                return Ok(Target::Workspace(workspace));
            }
        };
        let (kind, input) = item.unwrap();
        Ok(Target::Item {
            kind,
            input,
            repository,
            post,
        })
    }
}

/// The forge item an agent review covers, if any, and whether it posts there.
enum Subject {
    Changes,
    Pull {
        pull: PullRequest,
        post: Option<bool>,
    },
    Issue {
        issue: Issue,
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
        Target::Item {
            kind,
            input,
            repository,
            post,
        } => {
            let repo = item_repository(ctx, kind, &input, repository).await?;
            match kind {
                ItemKind::Pr => pull_request(ctx, &repo, &input, reviewer, post, args).await,
                ItemKind::Issue => issue(ctx, &repo, &input, reviewer, post, args).await,
            }
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

/// The registered repository of a PR or issue, from `--repo`, its URL, or the
/// current checkout, offering to register one that is missing.
async fn item_repository(
    ctx: &Context,
    kind: ItemKind,
    input: &str,
    repository: Option<String>,
) -> Result<Repository> {
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
        None if IssueInput::parse(input) == IssueInput::Url => {
            let forge = match kind {
                ItemKind::Pr => ForgeRepo::from_pull_url(input)?,
                ItemKind::Issue => ForgeRepo::from_issue_url(input)?,
            };
            super::issues::registered_remote(
                ctx,
                &mut repos,
                forge,
                "shoal review <url> --repo <repository>",
            )
            .await?
            .into()
        }
        None => super::issues::current_repository(ctx, repos.clone(), super::issues::REPO_OR_URL)
            .await?
            .into(),
    };
    Ok(crate::forge::repository::select(&repos, &repository)
        .await?
        .clone())
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
    let remote = crate::forge::repository::remote_url_from_path(&repo.path)
        .await?
        .context("PR review needs an origin remote")?;
    let pull = ForgeRepo::parse(&remote)?
        .pull_request(&repo.path, input)
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
                pr: Some(pull.url.clone()),
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
    let subject = Subject::Pull { pull, post };
    run(ctx, Some(workspace), reviewer, subject, args).await
}

/// Refine an issue before implementation in the workspace that works on it,
/// opening the one `shoal add --issue` would continue when none does.
async fn issue(
    ctx: &Context,
    repo: &Repository,
    input: &str,
    reviewer: Reviewer,
    post: Option<bool>,
    args: Vec<OsString>,
) -> Result<i32> {
    ensure!(
        !matches!(reviewer, Reviewer::Manual),
        "an issue review needs an agent; drop --manual"
    );
    let issue = super::issues::load(repo, input).await?;
    let workspace = match issue_workspace(ctx, repo, &issue).await? {
        Some(workspace) => workspace,
        None => {
            let creation = super::workspaces::Creation {
                path: None,
                branch: None,
                existing: None,
                base: None,
                git_profile: None,
                pr: None,
            };
            let code = super::workspaces::add(
                ctx,
                Some(repo.id.clone()),
                creation,
                Some(issue.url.clone()),
                super::workspaces::AgentLaunch::Explicit(None),
                Vec::new(),
                true,
            )
            .await?;
            if code != 0 {
                return Ok(code);
            }
            issue_workspace(ctx, repo, &issue)
                .await?
                .context("the opened issue workspace is missing")?
        }
    };
    eprintln!(
        "Reviewing issue #{} in workspace {}",
        issue.number, workspace.name
    );
    let reviewer = match reviewer {
        Reviewer::Ask => Reviewer::Agent(None),
        reviewer => reviewer,
    };
    let subject = Subject::Issue { issue, post };
    run(ctx, Some(workspace.id), reviewer, subject, args).await
}

/// The workspace associated with the issue, otherwise the one on its derived branch.
async fn issue_workspace(
    ctx: &Context,
    repo: &Repository,
    issue: &Issue,
) -> Result<Option<Workspace>> {
    let branch = issue.branch_name();
    let mut derived = None;
    let workspaces = client::workspaces(&ctx.paths).await?;
    for workspace in workspaces
        .into_iter()
        .filter(|workspace| workspace.repository_id == repo.id)
    {
        let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
        if inspection
            .issue
            .is_some_and(|linked| linked.url == issue.url)
        {
            return Ok(Some(workspace));
        }
        if workspace.branch == branch {
            derived = Some(workspace);
        }
    }
    Ok(derived)
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
    subject: Subject,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    let post = match &subject {
        Subject::Changes => None,
        Subject::Pull { post, .. } | Subject::Issue { post, .. } => *post,
    };
    let reviewer = match reviewer {
        // Only an agent posts, so choosing whether to post chooses the agent.
        Reviewer::Ask if post.is_some() => Reviewer::Agent(None),
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
            let post = post.unwrap_or(settings.review.post);
            let prompt = prompt(&workspace, &subject, post);
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

fn prompt(workspace: &Workspace, subject: &Subject, post: bool) -> String {
    let (subject, delivery) = match subject {
        Subject::Issue { issue, .. } => {
            let delivery = if post {
                format!(
                    "Post this as one comment on issue #{} without editing the issue. Do not \
                     edit files, commit, or push.",
                    issue.number
                )
            } else {
                LOCAL.to_owned()
            };
            return format!(
                "Review issue #{}: {}\n{}\n\n{}\n\nRefine the issue before implementation \
                 starts; this workspace is on branch {}. Check it against the code and report \
                 what is unclear, missing, inconsistent or already done, the code it affects, \
                 open questions, and a suggested approach. {delivery}",
                issue.number, issue.title, issue.url, issue.details, workspace.branch
            );
        }
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
mod tests {
    use super::*;

    #[test]
    fn urls_and_item_flags_select_a_forge_item_and_forge_options_require_one() {
        let url = "https://forge.example/team/repo/pulls/7";
        for (workspace, pr) in [(Some(url), None), (None, Some("7"))] {
            let target = Target::new(
                workspace.map(String::from),
                pr.map(|pr| (ItemKind::Pr, pr.to_owned())),
                Some("repo".into()),
                Some(false),
            )
            .unwrap();
            assert!(matches!(
                target,
                Target::Item {
                    kind: ItemKind::Pr,
                    repository: Some(_),
                    ..
                }
            ));
        }
        let issue = "https://forge.example/team/repo/issues/12";
        assert!(matches!(
            Target::new(Some(issue.into()), None, None, None).unwrap(),
            Target::Item {
                kind: ItemKind::Issue,
                ..
            }
        ));
        let branch = "https://forge.example/team/repo/src/branch/main";
        assert!(Target::new(Some(branch.into()), None, None, None).is_err());
        assert!(matches!(
            Target::new(Some("fix-login".into()), None, None, None).unwrap(),
            Target::Workspace(Some(name)) if name == "fix-login"
        ));
        for (repository, post) in [(Some("repo".into()), None), (None, Some(true))] {
            assert!(Target::new(Some("fix-login".into()), None, repository, post).is_err());
        }
    }
}
