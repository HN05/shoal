//! Starting a workspace from a pasted issue, PR or branch link, or an issue
//! number.
use std::ffi::OsString;

use anyhow::{Result, ensure};

use super::{AddOptions, AgentLaunch, reads::DaemonReads};
use crate::{
    agent::Agent,
    cli::{client, commands::issues, context::Context, ui},
    forge::items::reference::ItemInput,
    launches::workspace,
    start_plan::Creation,
};
use shoal_protocol::launches::StartTarget;

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
    let positional = positional.decided(names_branch)?;
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
    workspace::ensure_link_in(repo, linked.as_ref()).await?;
    match positional {
        StartTarget::Pull(pull) => {
            let context = issues::read_context(ctx, repo).await?;
            let pull = client::pull_request(&ctx.paths, context, pull.into()).await?;
            workspace::pull_creation(&DaemonReads(ctx), repo, pull, creation).await?;
        }
        StartTarget::Branch { branch, .. } => workspace::branch_creation(&branch, creation).await?,
        StartTarget::Repository(_) | StartTarget::Number(_) | StartTarget::Issue(_) => {
            unreachable!("returned above")
        }
    }
    Ok(Request {
        repository: Some(repo.id.clone().into()),
        issue: None,
        agent: AgentLaunch::Add(agent),
    })
}
