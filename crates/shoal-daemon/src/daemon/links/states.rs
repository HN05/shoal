//! The state on the forge of what a space's status lists: the PRs its PR
//! cleanup watches and its assigned issues.
use anyhow::Result;
use shoal_daemon_base::forge_access::ForgeHosts;
use shoal_forge::{RepoRequests, items::issue::IssueRequests, transport::account::Account};
use shoal_protocol::links::{issue, pr::Registration};
use shoal_protocol::status::IssueStatus;

use super::pr::{self, watched};
use crate::{
    daemon::manager::Manager,
    forge::{ForgeRepo, activity::status::PrStatus, items::reference::ItemRef},
};

impl Manager {
    /// The state of the PRs `pr_cleanup` watches and of the assigned
    /// `issues`, looked up on the forge of `remote`, the space's origin
    /// remote, as the user's logins read them.
    pub(crate) async fn linked_states(
        &self,
        remote: &Result<Option<String>>,
        pr_cleanup: Option<&Registration>,
        issues: &[issue::Registration],
    ) -> (Vec<PrStatus>, Vec<IssueStatus>) {
        let account = self.forges.user_account();
        let hosts = self.forges.hosts();
        tokio::join!(
            watched(&hosts, &account, remote, pr_cleanup),
            issue_states(&hosts, &account, remote, issues)
        )
    }
}

/// The state of each of the assigned `issues` on the forge of `remote`, the
/// space's origin remote.
async fn issue_states(
    hosts: &ForgeHosts<'_>,
    account: &Account,
    remote: &Result<Option<String>>,
    issues: &[issue::Registration],
) -> Vec<IssueStatus> {
    let states = (issues.iter()).map(|issue| issue_status(hosts, account, remote, &issue.url));
    futures_util::future::join_all(states).await
}

/// The state of the issue at `url` on the forge of `remote`.
async fn issue_status(
    hosts: &ForgeHosts<'_>,
    account: &Account,
    remote: &Result<Option<String>>,
    url: &ItemRef,
) -> IssueStatus {
    let state = async {
        let remote = pr::origin(remote, "issue lookup needs an origin remote")?;
        let forge = ForgeRepo::resolve(remote, hosts).await?;
        forge.state_of_issue(account, url).await
    }
    .await;
    let url = url.clone();
    match state {
        Ok(state) => IssueStatus {
            url,
            state: Some(state),
            error: None,
        },
        Err(error) => IssueStatus {
            url,
            state: None,
            error: Some(format!("{error:#}")),
        },
    }
}
