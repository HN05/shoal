use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn a_thread_is_linked_as_related_and_unlinked_with_or_without_it() {
    let number = |words: &[&str], linked| linked_thread(&args(words), linked);
    assert_eq!(
        number(&["thread", "3.2", "related"], true)
            .unwrap()
            .to_string(),
        "3.2"
    );
    assert!(number(&["thread", "3.2", "assigned"], true).is_err());
    assert!(number(&["thread", "3.2"], true).is_err());
    assert_eq!(
        number(&["thread", "3.2"], false).unwrap().to_string(),
        "3.2"
    );
    // Linking names another space's thread by its full number.
    assert!(number(&["thread", "2", "related"], true).is_err());
}

#[test]
fn resolving_needs_an_outcome_unless_it_reopens() {
    assert!(resolving(None, None, None, None, false).is_err());
    let reopen = resolving(None, None, None, None, true).unwrap();
    assert!(reopen.outcome.is_none());
    let answered = resolving(Some(OutcomeKind::Answered), Some(2), None, None, false).unwrap();
    assert_eq!(
        (answered.outcome, answered.choice),
        (Some(OutcomeKind::Answered), Some(2))
    );
}

#[test]
fn a_thread_shows_whose_turn_it_is_or_how_it_ended() {
    let thread = |posts: serde_json::Value, resolution: Option<serde_json::Value>| {
        let mut thread = serde_json::json!({
            "id": 1, "space_number": 2, "item": 3, "space_id": "w1", "space": "cache",
            "kind": "question", "name": "Which layout?", "opener": "space",
            "holder": {"kind": "user"}, "created_at": 0, "posts": posts,
        });
        if let Some(resolution) = resolution {
            thread["resolution"] = resolution;
        }
        serde_json::from_value::<shoal_protocol::threads::Thread>(thread).unwrap()
    };
    let post = |side: &str| serde_json::json!({"id": 1, "created_at": 0, "side": side, "sender": "space", "text": "A?"});
    assert_eq!(
        render::status(&thread(serde_json::json!([]), None)),
        "empty"
    );
    let asked = thread(serde_json::json!([post("opener")]), None);
    assert_eq!(render::status(&asked), "waiting for the user");
    let answered = thread(serde_json::json!([post("holder")]), None);
    assert_eq!(render::status(&answered), "waiting for cache");
    let withdrawn = serde_json::json!({
        "outcome": "withdrawn", "by": "space", "resolved_at": 0,
    });
    let resolved = thread(serde_json::json!([post("opener")]), Some(withdrawn));
    assert_eq!(render::status(&resolved), "withdrawn");
}
