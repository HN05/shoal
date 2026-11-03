//! What the daemon reads from a forge for a space or a registered checkout,
//! so a scoped caller reads as its agent account and anyone else with the
//! user's own logins.
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::{ForgeRepo, account::Account},
    protocol::ItemContext,
};

impl Manager {
    /// The account a read goes out as: the agent account for a `scoped`
    /// caller and the user's own logins otherwise.
    pub(in crate::daemon::links) fn read_account(&self, scoped: bool) -> Account {
        if scoped {
            self.agent_account()
        } else {
            self.user_account()
        }
    }

    /// The forge of the repository `context` runs in, with a workspace's
    /// worktree verified.
    pub(in crate::daemon::links) async fn context_forge(
        &self,
        context: &ItemContext,
    ) -> Result<ForgeRepo> {
        let place = self.place(context).await?;
        self.place_forge(&place).await
    }
}
