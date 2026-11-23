use super::*;
use crate::activity::ActivityRequests;
use serde_json::json;

#[test]
fn lists_need_numbers() {
    let forge = ForgeRepo::parse("https://gitlab.com/team/repo").unwrap();
    assert_eq!(
        listed_number(&forge, &json!({"iid": 3, "id": 9})).unwrap(),
        3
    );
    let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    assert!(listed_number(&forge, &json!({"number": 0})).is_err());
    assert!(listed_number(&forge, &json!({"title": "Bug"})).is_err());
}

#[test]
fn fingerprints_key_items_by_id_and_ignore_other_fields() {
    let read = |item: serde_json::Value| {
        fingerprints(&[item], &["body"], |item| {
            item["user"].as_str().unwrap_or_default().to_owned()
        })
    };
    let comment = json!({"id": 1, "body": "hi", "reactions": 1, "user": "alice"});
    let reacted = json!({"id": 1, "body": "hi", "reactions": 2, "user": "alice"});
    assert_eq!(read(comment.clone()).unwrap(), read(reacted).unwrap());
    let edited = read(json!({"id": 1, "body": "edited", "user": "alice"})).unwrap();
    assert_eq!(edited[0].0, "1");
    assert_eq!(edited[0].1.author, "alice");
    assert_ne!(read(comment).unwrap(), edited);
    assert!(read(json!({"body": "hi"})).is_err());
}

#[tokio::test]
async fn a_prs_discussion_reviews_and_checks_are_read_together() {
    use crate::{
        test_support::{Server, agent_on, repo_on, together},
        transport::memory::Memory,
    };
    use shoal_forge_types::backends::ForgeKind;
    let arrived = together(3);
    let server = Server::start(move |request| {
        let body = if request.line == "GET /api/v1/repos/team/repo/pulls/7" {
            json!({"number": 7, "title": "Fix", "state": "open", "mergeable": true,
                "head": {"ref": "topic", "sha": "abc"}, "base": {"ref": "main"}})
        } else if !arrived() {
            // A refusal that is not sent again, so a read alone fails.
            return (400, Vec::new(), Vec::new());
        } else if request.line.contains("/status") {
            json!({"statuses": [], "total_count": 0})
        } else {
            json!([])
        };
        (200, Vec::new(), body.to_string().into())
    });
    let account = agent_on(&server, "agent").remembering(Memory::default());
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let snapshot = forgejo
        .activity(&account, PullNumber::new(7))
        .await
        .unwrap();
    assert!(snapshot.errors.is_empty(), "{:?}", snapshot.errors);
    assert_eq!(server.received().len(), 4);
}
