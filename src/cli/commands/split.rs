//! `shoal split`: a child workspace on a new branch, which the parent
//! workspace's agent works on and receives the activity of.
use anyhow::{Context as _, Result};

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    model::{OpenedWorkspace, Workspace},
    protocol::Method,
};

pub(super) async fn run(
    ctx: &Context,
    workspace: Option<String>,
    branch: Option<String>,
    issue: Option<String>,
    base: Option<String>,
) -> Result<i32> {
    let parent = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let issue = match issue {
        Some(input) => Some(load_issue(ctx, &parent, &input).await?),
        None => None,
    };
    let name = match (branch, &issue) {
        (Some(branch), _) => branch,
        (None, Some(issue)) => issue.branch_name(),
        (None, None) => anyhow::bail!("name the child's branch or pass --issue"),
    };
    let child: Workspace = request(
        &ctx.paths,
        Method::SplitWorkspace {
            workspace: parent,
            name,
            base,
        },
    )
    .await?;
    if let Some(issue) = &issue {
        request::<()>(
            &ctx.paths,
            Method::SetIssue {
                workspace: child.id.clone(),
                url: issue.url.clone(),
                title: Some(issue.title.clone()),
            },
        )
        .await?;
    }
    let opened = OpenedWorkspace {
        workspace: child,
        reused: false,
    };
    let navigate = terminal::follows_caller(ctx) && !crate::env::is_scoped();
    let ready = super::workspaces::finish_add_workspace(ctx, opened, None, navigate).await?;
    Ok(if ready.is_some() { 0 } else { 1 })
}

/// The issue `input` names in the parent workspace's repository.
async fn load_issue(ctx: &Context, parent: &str, input: &str) -> Result<super::issues::Issue> {
    let workspace = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.id == parent || workspace.name == parent)
        .with_context(|| format!("unknown workspace: {parent}"))?;
    let repository = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repository| repository.id == workspace.repository_id)
        .context("the parent workspace's repository is not registered")?;
    super::issues::load(&ctx.forge_account()?, &repository, input).await
}

/// The parent and child workspaces, when the workspace has any.
pub(super) fn render(workspace: &Workspace) {
    if let Some(parent) = &workspace.parent_workspace {
        println!("Parent workspace: {}", super::base::describe(parent));
    }
    if !workspace.child_workspaces.is_empty() {
        let children: Vec<_> = workspace
            .child_workspaces
            .iter()
            .map(super::base::describe)
            .collect();
        println!("Child workspaces: {}", children.join(", "));
    }
}
