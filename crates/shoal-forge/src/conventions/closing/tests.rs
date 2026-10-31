use super::*;
use crate::items::{item::ItemState, link::ItemKind};

const URL: &str = "https://forge.example/team/repo/issues/7";

#[test]
fn a_closing_keyword_before_the_issue_closes_it() {
    for body in [
        "Closes #7",
        "## Summary\n\nFix it.\n\nfixes: #7.",
        "Resolved  #7, #9",
        "Part of #3; closes #7",
        "Closes https://forge.example/team/repo/issues/7",
        "→Fixes #7",
    ] {
        assert!(closes(body, 7, URL), "{body}");
    }
}

#[test]
fn other_references_leave_the_issue_open() {
    for body in [
        "",
        "Part of #7",
        "Refs #7",
        "Closes #70",
        "Closes #3, #7",
        "Closes other/repo#7",
        "Prefixes #7",
        "Closes https://forge.example/team/repo/issues/70",
        "Closes https://forge.example/team/repo/issues/7/comments",
    ] {
        assert!(!closes(body, 7, URL), "{body}");
    }
}

#[test]
fn the_warning_names_the_edit_that_closes_the_issue() {
    let item = Item {
        kind: ItemKind::Pr,
        number: 12,
        url: "https://forge.example/team/repo/pulls/12".into(),
        title: "Fix".into(),
        state: ItemState::Open,
        labels: Vec::new(),
        assignees: Vec::new(),
        milestone: None,
        pr: None,
    };
    assert_eq!(warning(&item, "Closes #7", 7, URL), None);
    assert_eq!(
        warning(&item, "Refs #7", 7, URL).as_deref(),
        Some(
            "the description of PR #12 does not close issue #7, so merging it leaves the issue \
             open; shoal pr edit 12 --body-file adds `Closes #7` unless the PR leaves part of \
             the issue for later"
        )
    );
}
