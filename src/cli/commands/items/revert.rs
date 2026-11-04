//! `shoal pr revert`: open a PR that undoes a merged one for a workspace or
//! chat, through the forge where it reverts PRs, and otherwise from a new
//! workspace whose branch reverts the merge with `git revert`.
use anyhow::{Context as _, Result};

use super::{
    content::read_content,
    open::{print_warnings, publish, report_proposed},
};
use crate::client::output::items::Revert;
use crate::{
    cli::{
        args::changes::{RevertArgs, RevertedPr},
        client::{self, request},
        commands::split,
        context::Context,
        ui,
    },
    daemon::links::proposals::OpenOutcome,
    execution,
    forge::{
        capability::Capability,
        items::{
            create::PullOptions,
            revert::{MergedPull, revert_options},
        },
    },
    git::revert::{self as git_revert, Merge, Merged, Reverted},
    model::{Chat, PresentSpace, Workspace},
    protocol::{
        AssignmentMethod, ItemContext, ItemMethod, OperationMethod, RemovalMethod, WorkspaceMethod,
        operations::{StartedWorkspace, WorkspaceStart},
    },
    removal::{RemovalResult, UnsavedWork},
    repository::Selector,
};

pub(super) async fn revert(ctx: &Context, selector: RevertedPr, args: RevertArgs) -> Result<i32> {
    let options = revert_args(args)?;
    let space = ui::select_space(ctx, selector.space, ui::Fallback::CurrentDirectory).await?;
    let from = Reverting::of(ctx, space).await?;
    if from.forge_reverts(ctx).await? {
        let method = ItemMethod::RevertPull {
            space: from.id().to_owned(),
            item: selector.item,
            options,
        };
        return report(ctx, request(&ctx.paths, method).await?, None);
    }
    let method = ItemMethod::MergedPull {
        space: from.id().to_owned(),
        item: selector.item,
    };
    let merged: MergedPull = request(&ctx.paths, method).await?;
    let Some(reverting) = revert_workspace(ctx, &from, &merged).await? else {
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

/// The space a revert is opened from: a workspace, its worktree verified, or
/// a running chat, which acts in its repository's registered checkout.
#[allow(
    clippy::large_enum_variant,
    reason = "a lookup's result, read by the revert that made it"
)]
enum Reverting {
    Workspace(Box<Workspace>),
    Chat(Chat),
}

impl Reverting {
    /// The workspace or running chat `space` names.
    async fn of(ctx: &Context, space: String) -> Result<Self> {
        match client::present_space(&ctx.paths, &space).await? {
            Some(PresentSpace::Chat(chat)) => Ok(Self::Chat(chat)),
            _ => {
                let method = WorkspaceMethod::VerifyWorkspace {
                    workspace: space,
                    on_branch: false,
                };
                Ok(Self::Workspace(Box::new(
                    request(&ctx.paths, method).await?,
                )))
            }
        }
    }

    fn id(&self) -> &str {
        match self {
            Self::Workspace(workspace) => &workspace.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    /// Whether the forge of the space's repository reverts PRs.
    async fn forge_reverts(&self, ctx: &Context) -> Result<bool> {
        let context = match self {
            Self::Workspace(workspace) => ItemContext::Workspace(workspace.id.clone()),
            Self::Chat(chat) => ItemContext::Chat(chat.id.clone()),
        };
        let capabilities = client::capabilities(&ctx.paths, context, None).await?;
        Ok(capabilities.supports(Capability::RevertPrs))
    }
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
/// create, and one a chat opens when the revert comes from a chat; `None`
/// when setup failed.
async fn revert_workspace(
    ctx: &Context,
    from: &Reverting,
    merged: &MergedPull,
) -> Result<Option<Workspace>> {
    let pr = merged.item.pr.as_ref().context("only PRs revert")?;
    let name = format!("revert-{}", merged.item.number);
    let base = Some(format!("origin/{}", pr.base));
    let started = match from {
        Reverting::Workspace(workspace) if crate::env::is_scoped() => {
            split::start_child(
                ctx,
                workspace.id.clone(),
                name,
                base,
                None,
                Default::default(),
            )
            .await?
        }
        Reverting::Workspace(workspace) => {
            let repository = workspace.repository_id.clone();
            start_workspace(ctx, repository, name, base, None).await?
        }
        Reverting::Chat(chat) => {
            let repository = chat.repository_id.clone();
            start_workspace(ctx, repository, name, base, Some(chat.id.clone())).await?
        }
    };
    split::follow_preparation(ctx, started).await
}

/// Start a new workspace `name` of `repository` on `base`, opened by
/// `chat`, if any.
async fn start_workspace(
    ctx: &Context,
    repository: String,
    name: String,
    base: Option<String>,
    chat: Option<String>,
) -> Result<StartedWorkspace> {
    let method = OperationMethod::StartWorkspace {
        repository: Selector::Value(repository),
        start: WorkspaceStart::Name(name),
        base,
        path: None,
        git_profile: None,
        parent: chat,
        options: Default::default(),
        issue: None,
        pr: None,
        agent: None,
        env: Some(crate::env::without_secrets()),
    };
    request(&ctx.paths, method).await
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
    let method = AssignmentMethod::MarkDone {
        space: workspace.id.clone(),
        cleanup: true,
        integrated: false,
        results: Vec::new(),
        sender_chat: None,
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
        Revert {
            opened,
            workspace: workspace.cloned(),
        },
    )?;
    Ok(0)
}
