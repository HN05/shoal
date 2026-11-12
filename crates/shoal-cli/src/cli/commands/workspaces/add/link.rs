//! Starting a workspace from a pasted issue, PR or branch link, or an issue
//! number.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use super::{AddOptions, AgentLaunch};
use crate::{
    agent::Agent,
    cli::args::target::StartTarget,
    cli::{
        client,
        commands::{issues, review},
        context::Context,
        ui,
    },
    forge::{ForgeRepo, PullRequest, items::reference::ItemInput},
    git,
    model::Repository,
    protocol::PullHead,
    start_plan::Creation,
};

pub(in crate::cli::commands) struct AddInput {
    pub positional: Option<StartTarget>,
    pub repository: Option<crate::repository::Selector>,
    pub issue: Option<ItemInput>,
}

/// What the input asks [`super::add`] for besides the creation.
struct Request {
    repository: Option<crate::repository::Selector>,
    issue: Option<ItemInput>,
    agent: AgentLaunch,
}

pub(in crate::cli::commands) async fn add(
    ctx: &Context,
    input: AddInput,
    mut creation: Creation,
    agent: Option<Agent>,
    args: Vec<OsString>,
    options: AddOptions,
) -> Result<i32> {
    let request = request(ctx, input, &mut creation, agent).await?;
    super::add(
        ctx,
        request.repository,
        creation,
        request.issue,
        request.agent,
        args,
        options,
    )
    .await
}

/// Classify the positional input as a repository, an issue number or an
/// item or branch link, and complete `creation` for a PR or branch.
async fn request(
    ctx: &Context,
    input: AddInput,
    creation: &mut Creation,
    agent: Option<Agent>,
) -> Result<Request> {
    let AddInput {
        positional,
        repository,
        issue,
    } = input;
    let Some(positional) = positional else {
        return Ok(Request {
            repository,
            issue,
            agent: AgentLaunch::Add(agent),
        });
    };
    // A number beside a branch or issue of its own names a repository.
    let names_branch = creation.branch.is_some() || creation.existing.is_some() || issue.is_some();
    let positional = match positional {
        StartTarget::Issue(ItemInput::Number(number)) if names_branch => {
            StartTarget::Repository(number.to_string())
        }
        positional => positional,
    };
    if let StartTarget::Repository(named) = positional {
        ensure!(
            repository.is_none(),
            "a positional repository cannot be combined with --repo"
        );
        return Ok(Request {
            repository: Some(ui::repository_selector(named)?),
            issue,
            agent: AgentLaunch::Add(agent),
        });
    }
    ensure!(
        !names_branch,
        "an item or branch link cannot be combined with --branch or --issue"
    );
    let linked = positional.linked_repository().cloned();
    if let StartTarget::Issue(issue) = &positional
        && repository.is_some()
    {
        // Resolve like --issue, which offers to register an unknown repository.
        return Ok(Request {
            repository,
            issue: Some(issue.clone()),
            agent: AgentLaunch::IssueDefault(agent),
        });
    }
    let mut repos = crate::client::repositories::list(&ctx.paths).await?;
    let repository = match repository {
        Some(repository) => repository,
        None => match &linked {
            Some(linked) => issues::registered_remote(
                ctx,
                &mut repos,
                linked.clone(),
                "shoal start <link> --repo <repository>",
            )
            .await?
            .into(),
            None => issues::current_repository(ctx, repos.clone(), issues::REPO_OR_URL)
                .await?
                .into(),
        },
    };
    let repo = crate::repository::select(&repos, &repository).await?;
    if let StartTarget::Issue(issue) = positional {
        return Ok(Request {
            repository: Some(repo.id.clone().into()),
            issue: Some(issue),
            agent: AgentLaunch::IssueDefault(agent),
        });
    }
    ensure_link_in(repo, linked.as_ref()).await?;
    match positional {
        StartTarget::Pull(pull) => {
            let context = issues::read_context(ctx, repo).await?;
            let pull = client::pull_request(&ctx.paths, context, pull.into()).await?;
            pull_creation(ctx, repo, pull, creation).await?;
        }
        StartTarget::Branch { branch, .. } => {
            git::check_branch_name(None, &branch).await?;
            creation.existing = Some(git::remote_ref("origin", &branch));
        }
        StartTarget::Repository(_) | StartTarget::Issue(_) => {
            unreachable!("returned above")
        }
    }
    Ok(Request {
        repository: Some(repo.id.clone().into()),
        issue: None,
        agent: AgentLaunch::Add(agent),
    })
}

/// Refuse a link that is not of `repo`, the repository of its checkout's
/// origin.
async fn ensure_link_in(repo: &Repository, linked: Option<&ForgeRepo>) -> Result<()> {
    let remote = crate::repository::remote_url_from_path(&repo.path)
        .await?
        .context("forge lookup needs an origin remote")?;
    ensure!(
        linked.is_some_and(|linked| linked.is_remote(&remote)),
        "link belongs to a different repository"
    );
    Ok(())
}

/// Reopen the workspace that owns the PR's head branch, or open the pushed
/// branch compared against the PR's base; either way the PR is linked.
pub(in crate::cli::commands) async fn pull_creation(
    ctx: &Context,
    repo: &Repository,
    pull: PullRequest,
    creation: &mut Creation,
) -> Result<()> {
    // Without a name of its own, a new workspace takes the PR's title.
    (creation.options.name).get_or_insert_with(|| {
        crate::client::space_names::SpaceName::derive(&pull.title, "workspace")
    });
    if pull.fork {
        ensure!(
            creation.path.is_none() && creation.git_profile.is_none() && creation.base.is_none(),
            "PR #{} comes from a fork and opens at its head in a workspace Shoal places; --path, --git-profile and --base apply to a branch",
            pull.number
        );
        creation.pull_head = Some(fork_head(repo, &pull).await?);
        creation.pr = Some(pull.url.into());
        return Ok(());
    }
    let owned = crate::client::workspaces::list(&ctx.paths)
        .await?
        .iter()
        .any(|workspace| {
            workspace.repository_id == repo.id
                && workspace.branch.as_deref() == Some(pull.head.as_str())
        });
    if owned {
        creation.existing = Some(pull.head.into());
    } else {
        if creation.base.is_none() {
            let base = pull.base.as_str();
            creation.base = Some(review::fetch_pull_branch(&repo.path, "base", base).await?);
        }
        creation.existing = Some(git::remote_ref("origin", pull.head.as_str()));
    }
    creation.pr = Some(pull.url.into());
    Ok(())
}

/// The head of `pull`, a PR from a fork in `repo`, which only the ref the
/// forge serves it under reaches, with its base branch fetched for its
/// workspace's diff to compare with.
pub(in crate::cli::commands) async fn fork_head(
    repo: &Repository,
    pull: &PullRequest,
) -> Result<PullHead> {
    let reference = pull.head_ref.clone().with_context(|| {
        format!(
            "PR #{} comes from a fork, and its forge serves no ref for a fork's head, so Shoal cannot open it",
            pull.number
        )
    })?;
    let commit = pull
        .head_commit
        .clone()
        .with_context(|| format!("PR #{} reports no head commit", pull.number))?;
    let base = review::fetch_pull_branch(&repo.path, "base", pull.base.as_str()).await?;
    Ok(PullHead {
        number: pull.number,
        reference,
        commit,
        base: Some(base),
    })
}
