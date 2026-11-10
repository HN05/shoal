use crate::client::space_ids::SpaceId;

use super::*;

#[test]
fn a_restored_chat_takes_the_issue_its_saved_chat_is_linked_to() {
    let repo = Repository {
        id: "repo".into(),
        path: "/repos/shoal".into(),
        source: "/repos/shoal".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: None,
        contribution: false,
    };
    let url = "https://forge.example/team/repo/issues/7";
    let chat: Chat = serde_json::from_value(serde_json::json!({
        "id": "c", "number": 4, "repository_id": "repo", "name": "shoal-plan-7",
        "agent": "claude", "mode": "plan", "pid": 1, "started_at": 0,
        "links": {"issue": url, "issue_title": "Decide", "prs": []},
    }))
    .unwrap();
    let chats = [chat];
    let restored = with_saved_issue(
        Topic::repository(&repo, &"plan".into()),
        &chats,
        &SpaceId::from("c"),
    );
    let issue = restored.linked_issue.unwrap();
    assert_eq!((issue.number, issue.url.as_str()), (7, url));
    let other = with_saved_issue(
        Topic::repository(&repo, &"plan".into()),
        &chats,
        &SpaceId::from("d"),
    );
    assert!(other.linked_issue.is_none());
}
