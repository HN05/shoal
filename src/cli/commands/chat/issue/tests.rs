use super::*;

#[test]
fn a_chat_on_an_issue_is_named_after_it_and_its_mode_without_asking() {
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
    let topic = on(Topic::repository(&repo, "plan"), &repo, &issue);
    assert_eq!(topic.mode, "plan");
    assert_eq!(topic.subject.unwrap().issue.unwrap().number, 1211);
    assert_eq!(topic.name, "shoal-plan-1211");
    assert!(!topic.asks_name);
    assert_eq!(
        topic.relaunch,
        [
            "start",
            "--chat",
            "--mode",
            "plan",
            "id",
            "--issue",
            "https://forge.example/team/shoal/issues/1211"
        ]
        .map(std::ffi::OsString::from)
    );
    assert_eq!(topic.links[0].0, Relation::Assigned);
}
