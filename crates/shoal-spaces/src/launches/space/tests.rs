use crate::launches::space::closed;
use shoal_forge::items::issue::ClosedIssue;

/// A named launch's fallback, which takes an issue it cannot read by its
/// URL, never takes a closed one: the forge's refusal is told apart from
/// a failed read, however a caller wraps it.
#[test]
fn a_closed_issue_is_told_apart_from_an_unread_one() {
    let refusal = anyhow::anyhow!(ClosedIssue).context("read issue 316");
    assert!(closed(&refusal));
    assert!(!closed(&anyhow::anyhow!("forge unreachable")));
}
