//! Viewing one of the issues and PRs a space links.
use anyhow::{Context as _, Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        Command,
        args::items::{ItemArgs, ItemsCommand, ViewArgs},
        context::Context,
        ui,
    },
    env,
    model::{LinkedItem, PresentSpace},
    refusal::{Reason, Refusal},
};

/// View one of the issues and PRs the space `workspace` names links, or
/// open it in the browser, as chosen.
pub(super) async fn view_command(ctx: &Context, space: Option<SpaceSelector>) -> Result<Command> {
    let space = space.context("select a space to view its issues and PRs")?;
    let (items, kind) = linked_items(ctx, &space).await?;
    ensure!(!items.is_empty(), "the {kind} links no issue or PR");
    // Workspace processes cannot open the browser.
    let browse = !env::inherits_scope(&ctx.paths.state);
    let (url, web) = ui::pick_linked(ctx, &items, browse)?;
    Ok(Command::Items(ItemsCommand::View(ViewArgs {
        items: ItemArgs {
            kind_or_url: Some(url),
            item: None,
            space: Some(space),
        },
        repo: Default::default(),
        no_comments: false,
        check: None,
        job: None,
        job_id: None,
        web,
    })))
}

/// The issues and PRs the space `space` links in its own repository, the
/// only ones `shoal view` reads for it: a workspace's worktree's, or a chat's
/// registered checkout's, and none for a chat without a repository, which
/// links nothing; with the space's kind.
async fn linked_items(
    ctx: &Context,
    space: &SpaceSelector,
) -> Result<(Vec<LinkedItem>, &'static str)> {
    let unknown = || Refusal::new(Reason::NotFound, format!("no space {space}"));
    let (links, path, kind) = match crate::client::workspaces::present_space(&ctx.paths, space)
        .await?
    {
        Some(PresentSpace::Workspace(workspace)) => (workspace.links, workspace.path, "workspace"),
        Some(PresentSpace::Chat(chat)) if chat.repository_id.is_none() => {
            return Ok((Vec::new(), "chat"));
        }
        Some(PresentSpace::Chat(chat)) => {
            let repos = crate::client::repositories::list(&ctx.paths).await?;
            let checkout = (repos.into_iter())
                .find(|repo| Some(&repo.id) == chat.repository_id.as_ref())
                .ok_or_else(unknown)?;
            (chat.links, checkout.path, "chat")
        }
        None => return Err(unknown().into()),
    };
    let items = links.items();
    if items.is_empty() {
        return Ok((items, kind));
    }
    let remote = crate::repository::remote_url_from_path(&path)
        .await?
        .context("item lookup needs an origin remote")?;
    let items = (items.into_iter())
        .filter(|item| item.in_repository(&remote))
        .collect();
    Ok((items, kind))
}
