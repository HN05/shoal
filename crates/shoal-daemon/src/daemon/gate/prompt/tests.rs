use super::*;

fn space(items: &[String]) -> Space<'_> {
    Space {
        name: "Fix login",
        kind: "workspace",
        mode: Some("worker"),
        branch: Some("fix-login"),
        items,
    }
}

#[test]
fn the_opening_holds_the_rules_cases_and_space_and_nothing_of_the_request() {
    let rules = GateRules {
        instructions: vec!["Follow AGENTS.md.".into()],
        escalate: vec!["a push to a tag".into()],
    };
    let items = vec!["#12 Login fails (assigned, open)".to_owned()];
    let opening = opening(&rules, &space(&items));
    for part in [
        "Follow AGENTS.md.",
        "- a push to a tag",
        "Fix login",
        "fix-login",
        "#12",
    ] {
        assert!(opening.contains(part), "{part}: {opening}");
    }
    // Two requests of one space open the same way, so they share a cache.
    assert_eq!(opening, super::opening(&rules, &space(&items)));
}

#[test]
fn a_request_gives_its_commits_and_warnings_when_it_has_them() {
    let warnings = vec!["no label starting with type/".to_owned()];
    let request = |commits| Request {
        title: "3.2: push topic at abc to origin",
        target: "push",
        reason: "",
        specification: "{}",
        commits,
        warnings: &warnings,
        items: None,
    };
    let with = super::request(&request(Some("commit abc\n\n    Fix it\n")));
    assert!(with.contains("Fix it") && with.contains("type/"), "{with}");
    let without = super::request(&request(None));
    assert!(!without.contains("### Its commits"), "{without}");
    // A long log is cut at a character boundary.
    let long = "é".repeat(LOG_LIMIT);
    assert!(super::request(&request(Some(&long))).len() < 2 * LOG_LIMIT);
}
