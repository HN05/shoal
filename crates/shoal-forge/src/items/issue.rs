//! Look up issue state.
use anyhow::{Context, Result, ensure};

use crate::{
    ForgeKind, ForgeRepo, Query,
    account::Account,
    backends::Backend,
    gh,
    items::{item::ItemState, link::ItemKind},
};

impl ForgeRepo {
    pub async fn issue_closed(&self, account: &Account, number: u64) -> Result<bool> {
        Ok(self.issue_state(account, number).await? == ItemState::Closed)
    }

    /// An issue's state; a PR's number is refused.
    pub(crate) async fn issue_state(&self, account: &Account, number: u64) -> Result<ItemState> {
        match self.kind {
            ForgeKind::GitHub => {
                let id = number.to_string();
                let repo = format!("{}/{}", self.host, self.path);
                let args = [
                    "issue",
                    "view",
                    &id,
                    "--repo",
                    &repo,
                    "--json",
                    "number,state",
                ];
                let output = gh(account, &args, Query::Issue).await?;
                github_state(&output, number)
            }
            ForgeKind::Forgejo | ForgeKind::GitLab | ForgeKind::Bitbucket => Ok(self
                .read(account, ItemKind::Issue, number)
                .await
                .context(self.kind.login_hint())?
                .item
                .state),
        }
    }
}

pub(crate) fn require_open(state: ItemState) -> Result<()> {
    ensure!(
        state == ItemState::Open,
        "issue is already closed; reopen it before starting an issue workspace"
    );
    Ok(())
}

/// The state `gh issue view --json number,state` reports.
pub(crate) fn github_state(output: &str, number: u64) -> Result<ItemState> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "UPPERCASE")]
    enum State {
        Open,
        Closed,
    }
    #[derive(serde::Deserialize)]
    struct Issue {
        number: u64,
        state: State,
    }
    let issue: Issue = serde_json::from_str(output).context("invalid gh issue response")?;
    ensure!(issue.number == number, "gh returned a different issue");
    Ok(match issue.state {
        State::Open => ItemState::Open,
        State::Closed => ItemState::Closed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_requires_matching_number_and_known_state() {
        for (state, expected) in [("OPEN", ItemState::Open), ("CLOSED", ItemState::Closed)] {
            let output = serde_json::json!({"number": 316, "state": state}).to_string();
            assert_eq!(github_state(&output, 316).unwrap(), expected);
            assert!(github_state(&output, 1).is_err());
        }
        for output in [
            r#"{"number":316,"state":"MERGED"}"#,
            r#"{"number":316}"#,
            "",
        ] {
            assert!(github_state(output, 316).is_err());
        }
    }
}
