use super::*;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

#[test]
fn only_open_prs_at_the_workspaces_head_can_be_marked() {
    assert_eq!(unmarkable(ItemState::Open, Some(HEAD), Some(HEAD)), None);
    assert_eq!(
        unmarkable(ItemState::Merged, Some(HEAD), Some(HEAD)).as_deref(),
        Some("the PR is merged")
    );
    assert_eq!(
        unmarkable(ItemState::Closed, Some(HEAD), None).as_deref(),
        Some("the PR is closed")
    );
    assert_eq!(
        unmarkable(ItemState::Open, Some(OTHER), Some(HEAD)).as_deref(),
        Some("its head on the forge is fedcba987654, not HEAD 0123456789ab")
    );
    // A chat marks a PR at whatever head the forge reports.
    assert_eq!(unmarkable(ItemState::Open, Some(OTHER), None), None);
    assert!(unmarkable(ItemState::Open, None, None).is_some());
}
