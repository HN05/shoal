use super::*;

fn number(space: u32, item: u32) -> ItemNumber {
    ItemNumber {
        space: space.into(),
        item,
    }
}

fn post(id: i64, side: Side) -> Post {
    Post {
        id,
        created_at: crate::time::UnixTime::from_seconds(id),
        side,
        sender: Sender::Space,
        space: None,
        post: NewPost {
            text: format!("post {id}"),
            ..NewPost::default()
        },
        files: Vec::new(),
        in_chat: false,
        forwarded_to: None,
        alert_id: None,
    }
}

fn thread(opener: Sender, holder: Holder, posts: Vec<Post>) -> Thread {
    Thread {
        id: 1,
        number: number(2, 3),
        space_id: "w1".into(),
        space: "cache".into(),
        kind: ThreadKind::Question,
        name: "Which database?".into(),
        opener,
        holder,
        created_at: crate::time::UnixTime::from_seconds(1),
        posts,
        resolution: None,
    }
}

#[test]
fn an_empty_thread_waits_for_nobody() {
    let empty = thread(Sender::Space, Holder::User, Vec::new());
    assert_eq!(empty.awaits(), None);
    assert!(!empty.is_waiting());
    assert!(!empty.waits_for_user());
}

#[test]
fn a_thread_waits_for_the_side_that_did_not_post_last() {
    let asked = thread(Sender::Space, Holder::User, vec![post(1, Side::Opener)]);
    assert!(asked.is_waiting());
    assert!(asked.waits_for_user());
    let answered = thread(
        Sender::Space,
        Holder::User,
        vec![post(1, Side::Opener), post(2, Side::Holder)],
    );
    assert_eq!(answered.awaits(), Some(Side::Opener));
    assert!(!answered.is_waiting());
    assert!(!answered.waits_for_user());
}

#[test]
fn a_forward_leaves_the_thread_waiting_for_its_holder() {
    let mut forward = post(2, Side::Holder);
    forward.forwarded_to = Some(Holder::User);
    let forwarded = thread(
        Sender::Space,
        Holder::User,
        vec![post(1, Side::Opener), forward],
    );
    assert!(forwarded.is_waiting());
}

#[test]
fn a_reply_to_the_user_s_thread_waits_for_the_user() {
    let space = Holder::Space(ThreadSpace {
        id: "w1".into(),
        name: None,
        number: None,
    });
    let replied = thread(
        Sender::User,
        space,
        vec![post(1, Side::Opener), post(2, Side::Holder)],
    );
    assert!(replied.waits_for_user());
}

#[test]
fn a_resolved_thread_waits_for_nobody() {
    let mut resolved = thread(Sender::Space, Holder::User, vec![post(1, Side::Opener)]);
    resolved.resolution = Some(Resolution {
        outcome: Outcome::Withdrawn { reason: None },
        by: Sender::Space,
        by_space: Some("w1".into()),
        resolved_at: crate::time::UnixTime::from_seconds(2),
    });
    assert_eq!(resolved.awaits(), None);
}

#[test]
fn a_thread_carries_its_resolution_beside_its_other_fields() {
    let mut resolved = thread(Sender::Space, Holder::User, vec![post(1, Side::Opener)]);
    resolved.resolution = Some(Resolution {
        outcome: Outcome::Duplicate { of: number(2, 1) },
        by: Sender::User,
        by_space: None,
        resolved_at: crate::time::UnixTime::from_seconds(5),
    });
    let json = serde_json::to_value(&resolved).unwrap();
    assert_eq!(json["number"], "2.3");
    assert_eq!(json["holder"]["kind"], "user");
    assert_eq!(json["posts"][0]["side"], "opener");
    assert_eq!(json["resolution"]["outcome"], "duplicate");
    assert_eq!(json["resolution"]["of"]["number"], "2.1");
    assert_eq!(serde_json::from_value::<Thread>(json).unwrap(), resolved);
}

#[test]
fn an_outcome_takes_only_its_own_data() {
    let question = ThreadKind::Question;
    assert_eq!(
        Outcome::new(question, OutcomeKind::Answered, Some(2), None, None).unwrap(),
        Outcome::Answered { choice: Some(2) }
    );
    assert!(Outcome::new(question, OutcomeKind::Duplicate, None, None, None).is_err());
    assert!(
        Outcome::new(
            question,
            OutcomeKind::Withdrawn,
            None,
            Some(number(1, 1)),
            None
        )
        .is_err()
    );
    assert!(Outcome::new(question, OutcomeKind::Closed, None, None, None).is_err());
}

#[test]
fn a_thread_s_status_follows_whose_turn_it_is_and_its_outcome() {
    let empty = thread(Sender::Space, Holder::User, Vec::new());
    assert_eq!(empty.status(), ThreadStatus::Waiting);
    let asked = thread(Sender::Space, Holder::User, vec![post(1, Side::Opener)]);
    assert_eq!(asked.status(), ThreadStatus::Queued);
    let replied = vec![post(1, Side::Opener), post(2, Side::Holder)];
    let mut replied = thread(Sender::Space, Holder::User, replied);
    assert_eq!(replied.status(), ThreadStatus::Waiting);
    replied.resolution = Some(Resolution {
        outcome: Outcome::Done,
        by: Sender::Space,
        by_space: None,
        resolved_at: crate::time::UnixTime::from_seconds(3),
    });
    assert_eq!(replied.status().to_string(), "done");
}

#[test]
fn a_request_is_done_and_a_question_answered() {
    let request = ThreadKind::Request;
    assert_eq!(
        Outcome::new(request, OutcomeKind::Done, None, None, None).unwrap(),
        Outcome::Done
    );
    assert!(Outcome::new(request, OutcomeKind::Done, Some(1), None, None).is_err());
    assert!(Outcome::new(request, OutcomeKind::Answered, None, None, None).is_err());
    assert!(Outcome::new(ThreadKind::Question, OutcomeKind::Done, None, None, None).is_err());
    let json = serde_json::to_value(Outcome::Done).unwrap();
    assert_eq!(json, serde_json::json!({"outcome": "done"}));
}

#[test]
fn a_bare_thread_number_names_the_caller_s_space() {
    let own: ThreadNumber = "4".parse().unwrap();
    assert_eq!(own, ThreadNumber::Own(4));
    assert_eq!(own.resolve(Some(7.into())).unwrap(), number(7, 4));
    assert!(own.resolve(None).is_err());
    let full: ThreadNumber = "3.2".parse().unwrap();
    assert_eq!(full.resolve(Some(7.into())).unwrap(), number(3, 2));
    assert!("0".parse::<ThreadNumber>().is_err());
    assert!("x".parse::<ThreadNumber>().is_err());
}

#[test]
fn a_thread_number_keeps_its_form_on_the_wire() {
    for number in [ThreadNumber::Own(4), ThreadNumber::Full(number(3, 2))] {
        let json = serde_json::to_string(&number).unwrap();
        assert_eq!(serde_json::from_str::<ThreadNumber>(&json).unwrap(), number);
    }
}
