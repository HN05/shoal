use super::*;

#[test]
fn a_blocker_is_an_issue_by_number_an_item_by_url_or_else_a_space() {
    let parsed = |input: &str| parse(input).unwrap();
    assert_eq!(parsed("12"), Blocker::Issue("12".into()));
    let issue = "https://github.com/team/app/issues/7";
    assert_eq!(parsed(issue), Blocker::Issue(issue.into()));
    let pr = "https://github.com/team/app/pull/8";
    assert_eq!(parsed(pr), Blocker::Pr(pr.into()));
    let run = "https://github.com/team/app/actions/runs/9";
    assert_eq!(parsed(run), Blocker::CiRun(run.into()));
    assert_eq!(parsed("api"), Blocker::Space("api".into()));
    assert!(parse("https://github.com/team/app/tree/topic").is_err());
}
