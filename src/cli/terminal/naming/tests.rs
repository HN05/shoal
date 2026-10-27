use super::*;

#[test]
fn tabs_without_an_issue_use_the_template_or_branch() {
    let repo = Repository {
        id: "repo".into(),
        path: "/repo".into(),
        source: "https://example.com/owner/shoal.git".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    };
    assert_eq!(tab_label(None, &repo, None, "topic"), "topic");
    assert_eq!(
        tab_label(Some("{repo}: {branch}".into()), &repo, None, "topic"),
        "shoal: topic"
    );
}
