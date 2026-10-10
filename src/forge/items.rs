//! Issue and PR actions the daemon carries out for a workspace, so its
//! scope rules apply and each change reaches the event journal.
use anyhow::{Context, Result, bail};

use super::{ForgeRepo, account::Account, action::Action, item::Item, link::ItemKind, repository};
use crate::{
    daemon::{events, workspace::Manager},
    model::Workspace,
};

impl Manager {
    /// Apply `action` to the item `input` names in the workspace's repository,
    /// or to its linked item of `kind`.
    pub async fn item_action(
        &self,
        selector: &str,
        kind: ItemKind,
        input: Option<String>,
        action: Action,
    ) -> Result<Item> {
        action.validate(kind)?;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = workspace_forge(&workspace).await?;
        let input = match input {
            Some(input) => input,
            None => linked(&workspace, kind)?,
        };
        let (number, url) = match kind {
            ItemKind::Issue => forge.issue(&input)?,
            ItemKind::Pr => forge.pull(&input)?,
        };
        let item = forge
            .act(&Account::user(), kind, number, &action)
            .await
            .with_context(|| format!("could not {} {url}", action.name()))?;
        let id = workspace.id.clone();
        let name = action.name();
        self.store
            .run(move |db| events::record_item(db, &id, kind, &url, name))
            .await?;
        Ok(item)
    }
}

/// The forge of the workspace's `origin`.
pub(super) async fn workspace_forge(workspace: &Workspace) -> Result<ForgeRepo> {
    let remote = repository::remote_url_from_path(&workspace.path)
        .await?
        .context("forge actions need an origin remote")?;
    ForgeRepo::parse(&remote)
}

fn linked(workspace: &Workspace, kind: ItemKind) -> Result<String> {
    match kind {
        ItemKind::Issue => workspace
            .links
            .issue
            .clone()
            .context("no issue is linked; pass a number or URL"),
        ItemKind::Pr => match workspace.links.prs.as_slice() {
            [url] => Ok(url.clone()),
            [] => bail!("no PR is linked; pass a number or URL"),
            _ => bail!("several PRs are linked; pass a number or URL"),
        },
    }
}
