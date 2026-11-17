use super::*;
use crate::forge::items::reference::ItemInput;

const ISSUE: &str = "https://github.com/team/repo/issues/316";

/// The open issue 316, titled.
fn open_issue() -> OpenIssue {
    OpenIssue {
        url: ISSUE.parse().unwrap(),
        title: "Show titles".into(),
        labels: Vec::new(),
    }
}

/// A start on an issue it read names it by its URL and links it once,
/// with its title, so a restore's issue fields and the inbox show it.
#[test]
fn a_start_on_a_read_issue_links_it_with_its_title() {
    let start = ChatStart {
        issue: Some(ItemInput::Number(316)),
        ..ChatStart::default()
    };

    let start = on_issue(on_issue(start, Some(open_issue())), Some(open_issue()));

    assert_eq!(start.issue, Some(ItemInput::Url(ISSUE.parse().unwrap())));
    let linked: Vec<_> = (start.links.issues.iter())
        .map(|issue| (issue.url.url(), issue.title.as_deref()))
        .collect();
    assert_eq!(linked, [(ISSUE, Some("Show titles"))]);
}

/// A start on an issue it could not read keeps it as named, for the
/// record to link without a title.
#[test]
fn a_start_on_an_unread_issue_keeps_it_as_named() {
    let start = ChatStart {
        issue: Some(ItemInput::Number(316)),
        ..ChatStart::default()
    };

    let start = on_issue(start, None);

    assert_eq!(start.issue, Some(ItemInput::Number(316)));
    assert!(start.links.issues.is_empty());
}
