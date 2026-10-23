//! Look up issue state.
use anyhow::{Result, ensure};

use crate::{ForgeRepo, account::Account, backends::Backend, items::item::ItemState};

impl ForgeRepo {
    pub async fn issue_closed(&self, account: &Account, number: u64) -> Result<bool> {
        Ok(self.issue_state(account, number).await? == ItemState::Closed)
    }

    /// The URLs of the open issues the forge records as blocking issue `number`.
    pub async fn open_issue_dependencies(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<Vec<String>> {
        self.kind
            .open_issue_dependencies(self, account, number)
            .await
    }

    /// An issue's state; a PR's number is refused.
    pub(crate) async fn issue_state(&self, account: &Account, number: u64) -> Result<ItemState> {
        self.kind.issue_state(self, account, number).await
    }
}

pub(crate) fn require_open(state: ItemState) -> Result<()> {
    ensure!(
        state == ItemState::Open,
        "issue is already closed; reopen it before starting an issue workspace"
    );
    Ok(())
}
