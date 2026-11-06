use super::*;

fn name(template: Option<&str>, issue: Option<(u64, &str)>) -> PlaceName {
    PlaceName {
        template: template.map(str::to_owned),
        repo: "shoal".into(),
        issue_number: issue.map(|(number, _)| number),
        issue_title: issue.map(|(_, title)| title.to_owned()),
    }
}

#[test]
fn places_take_the_template_then_the_issue_then_the_branch() {
    let issue = Some((375, "Fix login"));
    assert_eq!(name(None, None).render("topic", None), "topic");
    assert_eq!(name(None, issue).render("topic", None), "shoal#375");
    assert_eq!(
        name(Some("{repo}: {branch}"), None).render("topic", None),
        "shoal: topic"
    );
    assert_eq!(
        name(Some("{issue_number} {issue_title}"), issue).render("topic", None),
        "375 Fix login"
    );
    assert_eq!(name(Some("{issue_number}"), None).render("topic", None), "");
}

#[test]
fn a_title_replaces_the_default_label_and_fills_templates() {
    let issue = Some((375, "Fix login"));
    let title = Some("Login retries");
    assert_eq!(name(None, issue).render("topic", title), "Login retries");
    assert_eq!(name(None, None).render("topic", title), "Login retries");
    assert_eq!(
        name(Some("{repo}#{issue_number} {title}"), issue).render("topic", title),
        "shoal#375 Login retries"
    );
    // Without a title, `{title}` stands for the label a title replaces.
    assert_eq!(
        name(Some("{title}"), issue).render("topic", None),
        "shoal#375"
    );
    assert_eq!(name(Some("{title}"), None).render("topic", None), "topic");
}
