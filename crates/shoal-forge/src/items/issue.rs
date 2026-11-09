//! Look up issue state.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo, account::Account, backends::Backend, items::item::ItemState, numbers::IssueNumber,
};

/// An issue's title, description and labels, with the URL of its number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDetails {
    pub number: IssueNumber,
    pub url: String,
    pub title: String,
    pub details: String,
    pub labels: Vec<String>,
}

impl ForgeRepo {
    /// The issue `input` names, a number or URL in this repository.
    pub async fn read_issue(&self, account: &Account, input: &str) -> Result<IssueDetails> {
        let (number, url) = self.issue(input)?;
        let issue = self.issue_details(account, number).await?;
        Ok(IssueDetails {
            number,
            url,
            title: issue.item.title,
            details: issue.body,
            labels: issue.item.labels,
        })
    }

    /// Whether the issue at `url`, in this repository, is closed.
    pub async fn issue_url_closed(&self, account: &Account, url: &str) -> Result<bool> {
        let (number, _) = self.issue(url)?;
        Ok(self.issue_state(account, number).await? == ItemState::Closed)
    }

    /// The URLs of the open issues the forge records as blocking issue `number`.
    pub async fn open_issue_dependencies(
        &self,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Vec<String>> {
        self.kind
            .open_issue_dependencies(self, account, number)
            .await
    }

    /// An issue's state; a PR's number is refused.
    pub(crate) async fn issue_state(
        &self,
        account: &Account,
        number: IssueNumber,
    ) -> Result<ItemState> {
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
