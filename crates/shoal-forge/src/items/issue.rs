//! Look up issue state.
use anyhow::{Result, ensure};

use crate::{ForgeRepo, account::Account, backends::Backend, items::item::ItemState};

impl ForgeRepo {
    pub async fn issue_closed(&self, account: &Account, number: u64) -> Result<bool> {
        Ok(self.issue_state(account, number).await? == ItemState::Closed)
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
