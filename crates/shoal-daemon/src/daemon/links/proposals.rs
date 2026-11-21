//! Proposals' test fixtures for other crates, and their tests across
//! subsystems.
#[cfg(feature = "test-support")]
use anyhow::Result;
#[cfg(any(test, feature = "test-support"))]
use shoal_links::{proposals::proposal, space::LinkSpace};
#[cfg(any(test, feature = "test-support"))]
use shoal_protocol::approvals::{ISSUE, IssueSpecification, ProposedAt};

#[cfg(any(test, feature = "test-support"))]
use crate::{
    daemon::{
        approvals::{ApprovalRequest, Specification, Target},
        manager::Manager,
    },
    forge::items::create::NewIssue,
};

/// The space `selector` names as links see it, which other crates' tests
/// pass to [`store_issue_proposal`].
#[cfg(feature = "test-support")]
pub async fn link_space(
    manager: &Manager,
    selector: &shoal_protocol::space_names::SpaceSelector,
) -> Result<LinkSpace> {
    manager.links.link_space(manager, selector).await
}

/// Link the item `input` names to the space `selector` names, as the user
/// links it, for other crates' tests.
#[cfg(feature = "test-support")]
pub async fn link_item(
    manager: &Manager,
    selector: &shoal_protocol::space_names::SpaceSelector,
    kind: crate::forge::items::link::LinkKind,
    input: &crate::forge::items::reference::ItemInput,
    relation: crate::model::Relation,
    title: Option<String>,
) -> Result<shoal_protocol::links::Linked> {
    let link = manager
        .links
        .link_item(manager, selector, kind, input, relation, title, None);
    link.await
}

/// A pending issue proposal from `workspace`, which tests read.
#[cfg(any(test, feature = "test-support"))]
pub async fn store_issue_proposal(manager: &Manager, space: &LinkSpace) -> ApprovalRequest {
    let specification = Specification::Issue(IssueSpecification {
        issue: NewIssue {
            title: format!("Flaky in {}", space.name),
            ..NewIssue::default()
        },
        repository: None,
        link: false,
        workspace: space.worktree().map(|_| ProposedAt {
            branch: space.branch().map(shoal_refs::BranchName::from),
            commit: "head".into(),
        }),
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    });
    let mut request = proposal(&space.id, Target::IssueOpen, ISSUE, specification);
    request.repository_id = Some(space.repository_id().to_owned());
    let store = |tx: &rusqlite::Transaction<'_>| {
        crate::daemon::approvals::proposals::propose_issue(tx, request)
    };
    let (request, _) = manager.store.write(store).await.unwrap();
    request
}

#[cfg(test)]
pub(crate) mod tests;
