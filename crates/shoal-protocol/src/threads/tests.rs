use super::*;

fn number(space: u32, item: u32) -> ItemNumber {
    ItemNumber {
        space: space.into(),
        item,
    }
}

fn post(id: i64, side: Side) -> Post {
    on(id, Some(side))
}

/// A post on `side`, or a participant's on neither.
fn on(id: i64, side: Option<Side>) -> Post {
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
        code: Vec::new(),
        changes: Vec::new(),
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
        finding: None,
        name: "Which database?".into(),
        opener,
        opener_space: (opener == Sender::Space).then(|| ThreadSpace::here("w1".into())),
        holder,
        created_at: crate::time::UnixTime::from_seconds(1),
        posts,
        resolution: None,
        working: false,
        held: false,
        changes: Vec::new(),
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
fn a_thread_that_holds_its_posts_waits_for_nobody() {
    let mut held = thread(Sender::User, Holder::User, vec![post(1, Side::Opener)]);
    held.held = true;
    assert_eq!(held.awaits(), None);
    assert!(!held.is_waiting() && !held.waits_for_user());
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
fn a_participant_s_post_leaves_the_turn_where_it_was() {
    let joined = thread(
        Sender::Space,
        Holder::User,
        vec![post(1, Side::Opener), on(2, None)],
    );
    assert_eq!(joined.awaits(), Some(Side::Holder));
    assert!(joined.waits_for_user());
    assert_eq!(joined.last_turn().map(|post| post.id), Some(1));
    let replies: Vec<i64> = (joined.replies_to(1).unwrap())
        .map(|post| post.id)
        .collect();
    assert!(replies.is_empty());
}

#[test]
fn a_participant_s_post_is_answered_by_either_side() {
    let answered = thread(
        Sender::Space,
        Holder::User,
        vec![
            post(1, Side::Opener),
            on(2, None),
            on(3, None),
            post(4, Side::Opener),
        ],
    );
    let replies: Vec<i64> = (answered.replies_to(2).unwrap())
        .map(|post| post.id)
        .collect();
    assert_eq!(replies, [4]);
}

#[test]
fn a_participant_s_post_records_no_side() {
    let joined = thread(Sender::Space, Holder::User, vec![on(1, None)]);
    let json = serde_json::to_value(&joined).unwrap();
    assert!(json["posts"][0].get("side").is_none());
    assert_eq!(serde_json::from_value::<Thread>(json).unwrap(), joined);
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
        Outcome::new(question, OutcomeKind::Answered, None, None, None).unwrap(),
        Outcome::Answered
    );
    assert!(Outcome::new(question, OutcomeKind::Duplicate, None, None, None).is_err());
    let of = Some(number(1, 1));
    assert!(Outcome::new(question, OutcomeKind::Withdrawn, of, None, None).is_err());
    assert!(Outcome::new(question, OutcomeKind::Closed, None, None, None).is_err());
}

#[test]
fn only_a_review_thread_takes_a_finding_s_outcomes() {
    let reason = || Some("The caller checks the length".to_owned());
    let review = ThreadKind::Review;
    assert_eq!(
        Outcome::new(review, OutcomeKind::FalsePositive, None, reason(), None).unwrap(),
        Outcome::FalsePositive {
            reason: reason().unwrap()
        }
    );
    let issue = Some("#1240".to_owned());
    assert!(Outcome::new(review, OutcomeKind::Deferred, None, None, issue).is_ok());
    assert!(Outcome::new(review, OutcomeKind::WontFix, None, None, None).is_err());
    assert!(Outcome::new(review, OutcomeKind::Answered, None, None, None).is_err());
    let question = ThreadKind::Question;
    assert!(Outcome::new(question, OutcomeKind::Fixed, None, None, None).is_err());
}

#[test]
fn a_finding_s_outcome_is_stored_by_the_name_the_command_takes() {
    let outcome = Outcome::WontFix {
        reason: "Rare".into(),
    };
    let json = serde_json::to_value(&outcome).unwrap();
    assert_eq!(json["outcome"], OutcomeKind::WontFix.to_string());
    assert_eq!(serde_json::from_value::<Outcome>(json).unwrap(), outcome);
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
    // Its holder's agent switched to it, whoever's turn it is.
    replied.working = true;
    assert_eq!(replied.status(), ThreadStatus::Working);
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
    let reason = Some("Shipped".to_owned());
    assert!(Outcome::new(request, OutcomeKind::Done, None, reason, None).is_err());
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
