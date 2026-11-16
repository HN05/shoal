use super::*;

#[test]
fn a_chat_on_an_issue_is_named_after_its_title_without_asking() {
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
        url: "https://forge.example/team/shoal/issues/1211"
            .parse()
            .unwrap(),
        details: String::new(),
        labels: Vec::new(),
    };
    let plain = Topic::repository(&repo, &"plan".into());
    let home = crate::model::ChatHome::Checkout(&repo);
    assert_eq!(home.chat_name(&plain.mode), "shoal plan");
    let topic = on(plain, &issue);
    assert_eq!(topic.mode, "plan");
    assert_eq!(topic.subject.unwrap().issue.unwrap().number, 1211);
    assert_eq!(topic.name.as_deref(), Some("Attach to chats"));
    let super::super::Relaunch::Start(start) = topic.relaunch else {
        panic!("expected a start");
    };
    assert_eq!(
        (
            start
                .repository
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            start.issue
        ),
        (Some("id"), Some(Some(issue.url.clone().into())))
    );
    assert_eq!(topic.links.issues[0].url, issue.url);
}
