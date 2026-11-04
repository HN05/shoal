use super::*;

#[test]
fn the_oldest_issue_is_refused_beyond_the_listing_limit() {
    let item = |number: u64| -> Item {
        serde_json::from_value(serde_json::json!({
            "kind": "issue", "number": number, "url": "url", "title": "title",
            "state": "open", "labels": [],
        }))
        .unwrap()
    };
    let mut issues: Vec<_> = (0..MAX_LIMIT as u64 - 1).rev().map(item).collect();
    assert_eq!(oldest(&issues, "issues").unwrap().unwrap().number, 0);
    issues.insert(0, item(MAX_LIMIT as u64));
    assert!(oldest(&issues, "issues").is_err());
    assert!(oldest(&[], "issues").unwrap().is_none());
}

#[test]
fn a_plan_chat_is_named_after_its_issue_without_asking() {
    let repo = Repository {
        id: "id".into(),
        path: "/repo".into(),
        source: "https://forge.example/team/shoal.git".into(),
        last_used: 0,
        name: None,
        workspaces_dir: None,
        contribution: false,
    };
    let issue = Issue {
        number: 1211,
        title: "Attach to chats".into(),
        url: "https://forge.example/team/shoal/issues/1211".into(),
        details: String::new(),
    };
    let topic = topic(&repo, &issue);
    assert_eq!(topic.mode, "plan");
    assert_eq!(topic.subject.unwrap().issue.unwrap().number, 1211);
    assert_eq!(topic.name, "shoal-plan-1211");
    assert!(!topic.asks_name);
}
