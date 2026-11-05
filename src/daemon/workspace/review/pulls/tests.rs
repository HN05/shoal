use super::*;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

#[test]
fn only_open_prs_at_the_workspaces_head_can_be_marked() {
    assert_eq!(unmarkable(ItemState::Open, Some(HEAD), Some(HEAD)), None);
    assert!(unmarkable(ItemState::Merged, Some(HEAD), Some(HEAD)).is_some());
    assert!(unmarkable(ItemState::Open, Some(OTHER), Some(HEAD)).is_some());
    // A chat marks a PR at whatever head the forge reports.
    assert_eq!(unmarkable(ItemState::Open, Some(OTHER), None), None);
    assert!(unmarkable(ItemState::Open, None, None).is_some());
}
