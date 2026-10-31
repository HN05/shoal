//! `shoal pr revert`: open a PR that undoes a merged one, through the forge
//! where it reverts PRs, and otherwise from a new workspace whose branch
//! reverts the merge with `git revert`.
use anyhow::{Context as _, Result};
use serde::Serialize;

use super::{
    content::read_content,
    open::{print_warnings, publish, report_proposed},
};
use crate::{
    cli::{
        args::changes::{RevertArgs, RevertedPr},
        client::request,
        commands::{capabilities::forge_of, split},
        context::Context,
        ui,
    },
    daemon::links::proposals::OpenOutcome,
    execution,
    forge::{
        capability::Capability,
        items::{
            create::PullOptions,
            item::Opened,
            revert::{MergedPull, revert_options},
        },
    },
    git::revert::{self as git_revert, Merge, Merged, Reverted},
    model::Workspace,
    protocol::{AssignmentMethod, ItemMethod, RemovalMethod, WorkspaceMethod},
    removal::{RemovalResult, UnsavedWork},
    repository::Selector,
};

/// The revert and, when Git made it, the workspace its branch is in.
#[derive(Serialize)]
struct Report<'a> {
    #[serde(flatten)]
    opened: &'a Opened,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<&'a Workspace>,
}

pub(super) async fn revert(ctx: &Context, selector: RevertedPr, args: RevertArgs) -> Result<i32> {
    let options = revert_args(args)?;
    let workspace =
        ui::select_workspace(ctx, selector.workspace, ui::Fallback::CurrentDirectory).await?;
    let workspace: Workspace = request(
        &ctx.paths,
        WorkspaceMethod::VerifyWorkspace {
            workspace,
            on_branch: false,
        },
    )
    .await?;
    if forge_of(ctx, &workspace)
        .await?
        .supports(Capability::RevertPrs)
    {
        let method = ItemMethod::RevertPull {
            workspace: workspace.id,
            item: selector.item,
            options,
        };
        return report(ctx, request(&ctx.paths, method).await?, None);
    }
    let method = ItemMethod::MergedPull {
        workspace: workspace.id.clone(),
        item: selector.item,
    };
    let merged: MergedPull = request(&ctx.paths, method).await?;
    let Some(reverting) = revert_workspace(ctx, &workspace, &merged).await? else {
        return Ok(1);
    };
    let options = revert_options(&merged.item, options);
    let title = options.title.as_deref().unwrap_or_default();
    let message = format!("{title}\n\nThis reverts {}.", merged.item.url);
    if let Err(error) = revert_merge(ctx, &reverting, &merged, &message).await {
        remove(ctx, &reverting).await?;
        return Err(error.context(format!(
            "could not revert {}; removed workspace {}",
            merged.item.url, reverting.name
        )));
    }
    let outcome = publish(ctx, reverting.id.clone(), options)
        .await
        .with_context(|| {
            format!(
                "{} reverts {}, but opening its PR failed; retry with shoal pr open --workspace {}",
                reverting.name, merged.item.url, reverting.name
            )
        })?;
    finish(ctx, &reverting).await?;
    report(ctx, outcome, Some(&reverting))
}

/// What the revert is opened with, its files checked and its body read.
fn revert_args(args: RevertArgs) -> Result<PullOptions> {
    let (body, attachments) = read_content(args.body, args.files)?;
    Ok(PullOptions {
        title: args.title,
        body,
        base: None,
        draft: (args.draft || args.no_draft).then_some(args.draft),
        labels: args.labels,
        reviewers: args.reviewers,
        assignees: Vec::new(),
        milestone: None,
        attachments,
    })
}

/// A new workspace on branch `revert-<number>` from the remote's latest
/// base branch of the merged PR, set up as any new workspace is: a child of
/// a workspace process's own workspace, since that is the only kind it may
/// create; `None` when setup failed.
async fn revert_workspace(
    ctx: &Context,
    workspace: &Workspace,
    merged: &MergedPull,
) -> Result<Option<Workspace>> {
    let pr = merged.item.pr.as_ref().context("only PRs revert")?;
    let name = format!("revert-{}", merged.item.number);
    let base = Some(format!("origin/{}", pr.base));
    let created = if crate::env::is_scoped() {
        split::create_child(ctx, workspace.id.clone(), name, base, None).await?
    } else {
        let method = WorkspaceMethod::CreateWorkspace {
            path: None,
            repository: Selector::Value(workspace.repository_id.clone()),
            name,
            base,
            git_profile: None,
            chat: None,
        };
        request(&ctx.paths, method).await?
    };
    split::prepare(ctx, created).await
}

/// Revert the merge on the workspace's branch as tracked commands of that
/// workspace, once its worktree is verified, since setup ran in it.
/// A revert that conflicts needs a person or an agent to resolve, so it is
/// abandoned.
async fn revert_merge(
    ctx: &Context,
    workspace: &Workspace,
    merged: &MergedPull,
    message: &str,
) -> Result<()> {
    let workspace: Workspace = request(
        &ctx.paths,
        WorkspaceMethod::VerifyWorkspace {
            workspace: workspace.id.clone(),
            on_branch: true,
        },
    )
    .await?;
    let path = &workspace.path;
    let facts = Merged {
        merge_commit: &merged.merge_commit,
        head: &merged.head,
        commits: &merged.commits,
        head_ref: merged.head_ref.as_deref(),
    };
    let merge = Merge::of(path, &facts).await?;
    let run = async |command| execution::publish(&ctx.paths, workspace.id.clone(), command).await;
    match git_revert::revert(path, &merge, message, run).await? {
        Reverted::Committed => Ok(()),
        Reverted::Conflicts(paths) => anyhow::bail!(
            "the base branch has changed these files since the merge: {}",
            paths.join(", ")
        ),
    }
}

async fn remove(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let method = RemovalMethod::RemoveWorkspace {
        workspace: workspace.id.clone(),
        unsaved: UnsavedWork::All,
    };
    request::<RemovalResult>(&ctx.paths, method).await.map(drop)
}

/// Record the revert workspace's assignment as done, so cleanup removes it
/// once its PR merges.
async fn finish(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let method = AssignmentMethod::WorkspaceDone {
        workspace: workspace.id.clone(),
        cleanup: true,
        integrated: false,
        results: Vec::new(),
    };
    request::<crate::model::Completion>(&ctx.paths, method)
        .await
        .map(drop)
}

fn report(ctx: &Context, outcome: OpenOutcome, workspace: Option<&Workspace>) -> Result<i32> {
    let opened = match outcome {
        OpenOutcome::Opened(opened) => *opened,
        OpenOutcome::Proposed(proposed) => return report_proposed(ctx, "Revert", &proposed),
    };
    print_warnings(&opened);
    let text = match workspace {
        Some(workspace) => format!(
            "Revert opened from workspace {}: {}",
            workspace.name, opened.item.url
        ),
        None => format!("Revert opened: {}", opened.item.url),
    };
    ctx.emit(
        &text,
        Report {
            opened: &opened,
            workspace,
        },
    )?;
    Ok(0)
}
