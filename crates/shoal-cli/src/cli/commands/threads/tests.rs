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

fn data(reason: Option<&str>) -> OutcomeData {
    OutcomeData {
        of: None,
        reason: reason.map(str::to_owned),
        issue: None,
    }
}

#[test]
fn resolving_needs_an_outcome_unless_it_reopens() {
    assert!(resolving(None, data(None), false).is_err());
    let reopen = resolving(None, data(None), true).unwrap();
    assert!(reopen.outcome.is_none());
    let withdrawn = resolving(Some(OutcomeKind::Withdrawn), data(Some("Moot")), false).unwrap();
    assert_eq!(
        (withdrawn.outcome, withdrawn.reason.as_deref()),
        (Some(OutcomeKind::Withdrawn), Some("Moot"))
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
    let mut held = asked.clone();
    held.held = true;
    assert!(render::status(&held).starts_with("pending"));
    let withdrawn = serde_json::json!({
        "outcome": "withdrawn", "by": "space", "resolved_at": 0,
    });
    let resolved = thread(serde_json::json!([post("opener")]), Some(withdrawn));
    assert_eq!(render::status(&resolved), "withdrawn");
}

#[test]
fn a_post_shows_its_code_where_it_is_now() {
    let code = serde_json::json!({
        "id": 1, "space": {"id": "w1", "name": "cache", "number": 3},
        "original": {"path": "src/parse.rs", "lines": {"start": 39, "end": 52}},
        "latest": {"path": "src/parser.rs", "lines": {"start": 41, "end": 55}},
        "snippet": "fn parse() {\n}", "state": "changed",
    });
    let code: shoal_protocol::code::CodeRef = serde_json::from_value(code).unwrap();
    assert_eq!(
        render::code_lines(&code),
        [
            "    Code: src/parser.rs:42-55 (changed) in 3 cache",
            "      fn parse() {",
            "      }",
        ]
    );
}
