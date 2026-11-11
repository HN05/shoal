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
fn places_take_the_name_or_the_template_filled_with_it() {
    let issue = Some((375, "Fix login"));
    assert_eq!(
        name(None, issue).render("topic", "Login retries"),
        "Login retries"
    );
    assert_eq!(
        name(Some("{repo}: {branch}"), None).render("topic", "Login retries"),
        "shoal: topic"
    );
    assert_eq!(
        name(Some("{repo}#{issue_number} {name}"), issue).render("topic", "Login retries"),
        "shoal#375 Login retries"
    );
    assert_eq!(
        name(Some("{issue_number} {issue_title}"), issue).render("topic", "x"),
        "375 Fix login"
    );
    assert_eq!(name(Some("{issue_number}"), None).render("topic", "x"), "");
    assert_eq!(name(Some("[{title}]"), None).render("topic", "x"), "[x]");
}
