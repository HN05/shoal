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
        labels: Vec::new(),
    };
    let topic = on(Topic::repository(&repo, &"plan".into()), &repo, &issue);
    assert_eq!(topic.mode, "plan");
    assert_eq!(topic.subject.unwrap().issue.unwrap().number, 1211);
    assert_eq!(topic.name, "shoal-plan-1211");
    let super::super::Relaunch::Start(start) = topic.relaunch else {
        panic!("expected a start");
    };
    assert_eq!(
        (start.repository.as_deref(), start.issue),
        (Some("id"), Some(Some(issue.url.clone())))
    );
    assert_eq!(topic.links[0].0, Relation::Assigned);
}
