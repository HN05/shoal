use super::*;

const MARKER: DraftMarker = DraftMarker {
    prefixes: &["WIP:", "[WIP]"],
    marker: "WIP:",
};

#[test]
fn draft_prefixes_ignore_case_and_need_the_whole_marker() {
    assert_eq!(MARKER.prefix_len("WIP: x"), Some(4));
    assert_eq!(MARKER.prefix_len("[wip] x"), Some(5));
    assert_eq!(MARKER.prefix_len("Wipe x"), None);
    assert_eq!(MARKER.prefix_len("Ü"), None);
}

#[test]
fn draft_titles_replace_any_prefix_with_the_marker() {
    assert_eq!(MARKER.draft_title("[wip]  Fix it", true), "WIP: Fix it");
    assert_eq!(MARKER.draft_title("Fix it", true), "WIP: Fix it");
    assert_eq!(MARKER.draft_title("WIP: Fix it", false), "Fix it");
    assert_eq!(MARKER.draft_title("Fix it", false), "Fix it");
}
