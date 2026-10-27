//! The work a queued addition waits for.
use anyhow::Result;

use super::{AddOptions, target::AddTarget};
use crate::{
    cli::{client::request, commands::issues, context::Context},
    forge::IssueInput,
    model::Dependency,
    protocol::WorkspaceMethod,
};

/// The work `--after` names, refused before any prompt when the repository
/// has no such workspace, queued agent or issue.
pub(super) async fn after_dependencies(
    ctx: &Context,
    target: &AddTarget,
    after: &[String],
) -> Result<Vec<Dependency>> {
    if after.is_empty() {
        return Ok(Vec::new());
    }
    let repo = target.repository(ctx).await?;
    let mut dependencies = Vec::new();
    for input in after {
        dependencies.push(match IssueInput::parse(input) {
            IssueInput::Number | IssueInput::Url => {
                Dependency::Issue(issues::url(ctx, repo, input).await?)
            }
            IssueInput::Invalid => Dependency::Work(input.clone()),
        });
    }
    request::<()>(
        &ctx.paths,
        WorkspaceMethod::CheckDependencies {
            repository: target.selector.clone(),
            after: dependencies.clone(),
        },
    )
    .await?;
    Ok(dependencies)
}

/// The work a launch waits for: `after`, and the open issues the forge
/// records as blocking its issue. A launch that starts now waits for nothing.
pub(super) async fn launch_dependencies(
    ctx: &Context,
    target: &AddTarget,
    issue: Option<&issues::Issue>,
    options: &AddOptions,
    mut after: Vec<Dependency>,
) -> Result<Vec<Dependency>> {
    if options.starts_now() {
        return Ok(Vec::new());
    }
    if let Some(issue) = issue {
        after.extend(issue_blockers(ctx, target, issue).await?);
    }
    Ok(after)
}

/// The open issues the forge records as blocking `issue`. A forge that
/// cannot list them, such as a repository with dependencies turned off,
/// leaves a warning and none.
async fn issue_blockers(
    ctx: &Context,
    target: &AddTarget,
    issue: &issues::Issue,
) -> Result<Vec<Dependency>> {
    let repo = target.repository(ctx).await?;
    match issues::open_dependencies(ctx, repo, issue).await {
        Ok(urls) => Ok(urls.into_iter().map(Dependency::Issue).collect()),
        Err(error) => {
            eprintln!(
                "warning: cannot list the issues blocking #{}, so the agent does not wait for them: {error:#}",
                issue.number
            );
            Ok(Vec::new())
        }
    }
}
