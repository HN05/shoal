//! Pasted links and item selectors share repository and workspace workflows.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use super::workspaces::{self, AgentLaunch, Creation};
use crate::{
    agent::Agent,
    cli::{client, context::Context, ui},
    forge::{
        ForgeRepo, IssueInput,
        link::{Link, LinkTarget},
    },
    git,
};

pub(super) struct AddInput {
    pub positional: Option<String>,
    pub repository: Option<String>,
    pub issue: Option<String>,
}

pub(super) async fn add(
    ctx: &Context,
    input: AddInput,
    mut creation: Creation,
    agent: Option<Agent>,
    args: Vec<OsString>,
    here: bool,
) -> Result<i32> {
    let Some(positional) = input.positional.as_deref() else {
        return workspaces::add(
            ctx,
            input.repository,
            creation,
            input.issue,
            AgentLaunch::Explicit(agent),
            args,
            here,
        )
        .await;
    };
    let number = IssueInput::parse(positional) == IssueInput::Number
        && creation.branch.is_none()
        && creation.existing.is_none()
        && input.issue.is_none();
    let pasted = ["/issues/", "/pull/", "/pulls/", "/tree/", "/src/branch/"]
        .iter()
        .any(|route| positional.contains(route))
        && IssueInput::parse(positional) == IssueInput::Url;
    if !number && !pasted {
        ensure!(
            input.repository.is_none(),
            "a positional repository cannot be combined with --repo"
        );
        return workspaces::add(
            ctx,
            input.positional,
            creation,
            input.issue,
            AgentLaunch::Explicit(agent),
            args,
            here,
        )
        .await;
    }
    ensure!(
        creation.branch.is_none() && creation.existing.is_none() && input.issue.is_none(),
        "an item or branch link cannot be combined with a branch, --existing or --issue"
    );
    let link = pasted.then(|| Link::parse(positional)).transpose()?;
    let mut repos = client::repositories(&ctx.paths).await?;
    let repository = match input.repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => match &link {
            Some(link) => super::issues::registered_remote(
                ctx,
                &mut repos,
                ForgeRepo::parse(&link.repository.repository_url())?,
                "shoal add <link> --repo <repository>",
            )
            .await?
            .into(),
            None => super::issues::repository_for_number(ctx, repos.clone())
                .await?
                .into(),
        },
    };
    let repo = crate::forge::repository::select(&repos, &repository).await?;
    let target = link.as_ref().map(|link| &link.target);
    if matches!(target, None | Some(LinkTarget::Issue)) {
        return workspaces::add(
            ctx,
            Some(repo.id.clone()),
            creation,
            Some(positional.into()),
            AgentLaunch::IssueDefault(agent),
            args,
            here,
        )
        .await;
    }
    let remote = crate::forge::repository::remote_url_from_path(&repo.path)
        .await?
        .context("link needs an origin remote")?;
    let forge = ForgeRepo::parse(&remote)?;
    ensure!(
        link.as_ref().is_some_and(|link| link.repository == forge),
        "link belongs to a different repository"
    );
    match target.unwrap() {
        LinkTarget::Pr => {
            let pull = forge.pull_request(&repo.path, positional).await?;
            let owned = client::workspaces(&ctx.paths)
                .await?
                .iter()
                .any(|workspace| {
                    workspace.repository_id == repo.id && workspace.branch == pull.head
                });
            if owned {
                creation.existing = Some(pull.head);
            } else {
                if creation.base.is_none() {
                    creation.base =
                        Some(super::review::fetch_pull_base(&repo.path, &pull.base).await?);
                }
                creation.existing = Some(git::remote_ref("origin", &pull.head));
            }
        }
        LinkTarget::Branch(branch) => {
            git::check_branch_name(None, branch).await?;
            creation.existing = Some(git::remote_ref("origin", branch));
        }
        LinkTarget::Issue => unreachable!(),
    }
    workspaces::add(
        ctx,
        Some(repo.id.clone()),
        creation,
        None,
        AgentLaunch::Explicit(agent),
        args,
        here,
    )
    .await
}
