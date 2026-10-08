//! Pasted links and item selectors share repository and workspace workflows.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use super::workspaces::{self, AgentLaunch, Creation};
use crate::{
    agent::Agent,
    cli::{ItemArgs, client, context::Context, ui},
    forge::{
        ForgeRepo, IssueInput,
        link::{ItemKind, Link, LinkTarget, Selection},
    },
    git,
    model::ReviewMark,
    protocol::Method,
};

pub(super) async fn link(ctx: &Context, items: ItemArgs) -> Result<i32> {
    let selected = Selection::parse(items.kind_or_url, items.item)?;
    let input = selected
        .input
        .context("link needs an issue or PR URL, or pr/issue and a number")?;
    let workspace =
        ui::select_workspace(ctx, items.workspace, ui::Fallback::CurrentDirectory).await?;
    match selected.kind.unwrap() {
        ItemKind::Pr => {
            workspaces::pr(
                ctx,
                Some(workspace),
                crate::forge::pr::Action::Watch { url: input },
            )
            .await
        }
        ItemKind::Issue => {
            client::request::<()>(
                &ctx.paths,
                Method::SetIssue {
                    workspace,
                    url: input,
                },
            )
            .await?;
            ctx.emit(
                "Issue linked; closure can complete the assignment.",
                serde_json::json!({"registered": true}),
            )?;
            Ok(0)
        }
    }
}

pub(super) async fn unlink(ctx: &Context, items: ItemArgs) -> Result<i32> {
    let selected = Selection::parse(items.kind_or_url, items.item)?;
    let workspace =
        ui::select_workspace(ctx, items.workspace, ui::Fallback::CurrentDirectory).await?;
    if selected.kind != Some(ItemKind::Pr) {
        client::request::<()>(
            &ctx.paths,
            Method::ClearIssue {
                workspace: workspace.clone(),
                url: selected.input.clone(),
            },
        )
        .await?;
    }
    if selected.kind != Some(ItemKind::Issue) {
        let action = selected
            .input
            .map_or(crate::forge::pr::Action::Clear, |url| {
                crate::forge::pr::Action::Unwatch { url }
            });
        client::request::<()>(&ctx.paths, Method::SetPr { workspace, action }).await?;
    }
    ctx.emit("Items unlinked", serde_json::json!({"registered": false}))?;
    Ok(0)
}

pub(super) async fn watch(ctx: &Context, items: ItemArgs, timeout: u64) -> Result<i32> {
    let selected = Selection::parse(items.kind_or_url, items.item)?;
    let workspace =
        ui::select_workspace(ctx, items.workspace, ui::Fallback::CurrentDirectory).await?;
    workspaces::watch_items(ctx, workspace, selected, timeout).await
}

pub(super) async fn ready(ctx: &Context, items: ItemArgs) -> Result<i32> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    let workspace =
        ui::select_workspace(ctx, items.workspace, ui::Fallback::CurrentDirectory).await?;
    let marks: Vec<ReviewMark> = client::request(
        &ctx.paths,
        Method::MarkReady {
            workspace,
            selection,
        },
    )
    .await?;
    ctx.emit(
        &format!("Ready for review:\n{}", review_lines(&marks)),
        &marks,
    )?;
    Ok(0)
}

pub(super) async fn unready(ctx: &Context, items: ItemArgs) -> Result<i32> {
    let selection = Selection::parse(items.kind_or_url, items.item)?;
    let workspace =
        ui::select_workspace(ctx, items.workspace, ui::Fallback::CurrentDirectory).await?;
    let marks: Vec<ReviewMark> = client::request(
        &ctx.paths,
        Method::ClearReady {
            workspace,
            selection,
        },
    )
    .await?;
    let message = if marks.is_empty() {
        "No ready-for-review marks".to_owned()
    } else {
        format!("Ready-for-review marks cleared:\n{}", review_lines(&marks))
    };
    ctx.emit(&message, &marks)?;
    Ok(0)
}

/// One indented line per mark: its item, or the workspace, and whether new
/// commits arrived since.
pub(super) fn review_lines(marks: &[ReviewMark]) -> String {
    marks
        .iter()
        .map(|mark| {
            let item = match (mark.kind, &mark.url) {
                (Some(kind), Some(url)) => format!("{kind} {url}"),
                _ => "workspace".to_owned(),
            };
            let outdated = if mark.stale == Some(true) {
                " (outdated)"
            } else {
                ""
            };
            format!("  {item}{outdated}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

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
            None => {
                super::issues::current_repository(ctx, repos.clone(), super::issues::REPO_OR_URL)
                    .await?
                    .into()
            }
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
