//! Look up issue state.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Items,
    items::{item::ItemState, link::LinkKind, reference::ItemRef},
    numbers::IssueNumber,
};

/// An issue's title, description and labels, with the URL of its number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDetails {
    pub number: IssueNumber,
    pub url: ItemRef,
    pub title: String,
    pub details: String,
    pub labels: Vec<String>,
}

impl ForgeRepo {
    /// The issue `issue`, of this repository: an open one, which work
    /// starts on, or with `closed` one in any state, such as the issue a
    /// workspace already has.
    pub async fn read_issue(
        &self,
        account: &Account,
        issue: &ItemRef,
        closed: bool,
    ) -> Result<IssueDetails> {
        let number = issue.issue()?;
        let details = match closed {
            true => self.issue_details(account, number).await?,
            false => self.open_issue_details(account, number).await?,
        };
        Ok(IssueDetails {
            number,
            url: issue.clone(),
            title: details.item.title,
            details: details.body,
            labels: details.item.labels,
        })
    }

    /// The open issue `number`, which must have a title; work starts on no
    /// closed issue.
    pub async fn open_issue_details(
        &self,
        account: &Account,
        number: IssueNumber,
    ) -> Result<crate::items::item::Current> {
        let issue = self.issue_details(account, number).await?;
        require_open(issue.item.state)?;
        Ok(issue)
    }

    /// Whether the issue `issue`, of this repository, is closed.
    pub async fn issue_closed(&self, account: &Account, issue: &ItemRef) -> Result<bool> {
        let state = self.issue_state(account, issue.issue()?).await?;
        Ok(state == ItemState::Closed)
    }

    /// The open issues the forge records as blocking issue `number`.
    pub async fn open_issue_blockers(
        &self,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Vec<ItemRef>> {
        let urls = self.kind.open_issue_blockers(self, account, number).await?;
        (urls.iter())
            .map(|url| ItemRef::parse_kind(LinkKind::Issue, url))
            .collect()
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

fn require_open(state: ItemState) -> Result<()> {
    ensure!(
        state == ItemState::Open,
        "issue is already closed; reopen it before starting an issue workspace"
    );
    Ok(())
}
