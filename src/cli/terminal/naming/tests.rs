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
    assert_eq!(name(None, None).render("topic"), "topic");
    assert_eq!(name(None, issue).render("topic"), "shoal#375");
    assert_eq!(
        name(Some("{repo}: {branch}"), None).render("topic"),
        "shoal: topic"
    );
    assert_eq!(
        name(Some("{issue_number} {issue_title}"), issue).render("topic"),
        "375 Fix login"
    );
    assert_eq!(name(Some("{issue_number}"), None).render("topic"), "");
}
