use super::*;
use crate::items::item::PrSummary;
use serde_json::json;

fn pull(head_repository: Option<&str>) -> Current {
    Current {
        item: Item {
            kind: ItemKind::Pr,
            number: 7,
            url: String::new(),
            title: "Fix".into(),
            state: ItemState::Open,
            labels: Vec::new(),
            pr: Some(PrSummary {
                head: "topic".into(),
                base: "main".into(),
                draft: false,
            }),
        },
        node_id: None,
        head_repository: head_repository.map(str::to_owned),
        body: String::new(),
        author: None,
        head_sha: None,
        base_sha: None,
        conflict: None,
        merges_now: false,
        reviewers: Vec::new(),
        user_ids: Default::default(),
        replaced_labels: Vec::new(),
    }
}

#[test]
fn only_branches_of_this_repository_can_be_opened() {
    let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    assert_eq!(
        branches(&forge, pull(Some("Team/Repo"))).unwrap(),
        ("Fix".into(), "topic".into(), "main".into())
    );
    for fork in [Some("someone/repo"), None] {
        let error = branches(&forge, pull(fork)).unwrap_err();
        assert!(error.to_string().contains("fork"), "{error}");
    }
}

#[test]
fn merged_commits_need_full_ids_and_lists_need_numbers() {
    let sha = "a".repeat(40);
    assert_eq!(commit_shas(&[json!({"id": sha})], "id").unwrap(), [sha]);
    for commit in [
        json!({"sha": "abc"}),
        json!({}),
        json!({"sha": "g".repeat(40)}),
    ] {
        assert!(commit_shas(&[commit], "sha").is_err());
    }
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
