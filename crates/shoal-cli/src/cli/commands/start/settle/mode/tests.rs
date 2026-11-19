use super::fit;
use shoal_protocol::launches::{IssueChoice, Launch};

/// A start on an issue, by number positionally or with `--issue`, picks
/// only among the modes that start in a repository, since a space
/// without one starts on no issue.
#[test]
fn a_start_on_an_issue_number_picks_only_modes_in_a_repository() {
    let positional = Launch {
        target: Some("37".parse().unwrap()),
        ..Launch::default()
    };
    let flagged = Launch {
        issue: Some(IssueChoice::Named("37".parse().unwrap())),
        ..Launch::default()
    };
    for launch in [positional, flagged] {
        assert_eq!(fit(&launch).unwrap().repository, Some(true), "{launch:?}");
    }
    assert_eq!(fit(&Launch::default()).unwrap().repository, None);
}
