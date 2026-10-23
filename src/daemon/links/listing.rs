//! Issues and PRs the daemon lists for a workspace, so a scoped caller's
//! search goes out as its agent account.
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::items::{item::Item, link::ItemKind, list::Search},
};

impl Manager {
    /// Issues or PRs of the workspace's repository that `search` matches,
    /// read as its agent account for a `scoped` caller and as the user's
    /// own logins otherwise.
    pub async fn list_items(
        &self,
        selector: &str,
        kind: ItemKind,
        search: &Search,
        scoped: bool,
    ) -> Result<Vec<Item>> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = self.workspace_forge(&workspace).await?;
        let account = if scoped {
            self.agent_account(&workspace).await?
        } else {
            self.user_account()
        };
        forge.list(&account, kind, search).await
    }
}
