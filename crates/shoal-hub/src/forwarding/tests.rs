use serde_json::json;

use super::*;

#[test]
fn a_forwarded_request_names_its_space_by_id() {
    let selector = SpaceSelector::Number(7.into());
    let id = SpaceId::from("space-seven".to_owned());
    let named = serde_json::to_value(&selector).unwrap();
    let mut method = json!({"send_alert": {"to": {"space": {"space": named}}, "message": "7"}});
    bind_space(&mut method, &selector, &id).unwrap();
    let bound = serde_json::to_value(SpaceSelector::id(&id)).unwrap();
    assert_eq!(method["send_alert"]["to"]["space"]["space"], bound);
    // Only what selects a space changes.
    assert_eq!(method["send_alert"]["message"], "7");
}

#[test]
fn binding_a_space_leaves_what_a_request_carries_alone() {
    use shoal_protocol::protocol::{CiMethod, ItemContext, Operation, RepoTarget};
    let selector = SpaceSelector::Number(7.into());
    let id = SpaceId::from("space-seven".to_owned());
    let context = ItemContext::Space {
        space: selector.clone(),
        repository: None,
    };
    let inputs = [("space".to_owned(), "7".to_owned())].into();
    let run = Operation::Ci(CiMethod::StartCiRun {
        target: RepoTarget::from(context),
        workflow: Some("ci.yml".into()),
        branch: None,
        inputs,
    });
    let mut method = serde_json::to_value(run).unwrap();
    bind_space(&mut method, &selector, &id).unwrap();
    let Operation::Ci(CiMethod::StartCiRun { target, inputs, .. }) =
        serde_json::from_value(method).unwrap()
    else {
        panic!("a CI run stays one");
    };
    let Some(ItemContext::Space { space, .. }) = target.context else {
        panic!("its target stays the space");
    };
    assert_eq!(space, SpaceSelector::id(&id));
    assert_eq!(inputs["space"], "7");
}

/// The frame a response with `body` travels in.
fn frame(body: Body) -> serde_json::Value {
    serde_json::to_value(Response::new(1, body)).unwrap()
}

#[test]
fn a_paged_answer_from_another_machine_arrives_whole() {
    let hold = |name: &str| -> shoal_protocol::model::SpaceHold {
        serde_json::from_value(json!({
            "space_id": "w1", "name": name, "reason": null, "created_at": 0, "placed_by": "user",
        }))
        .unwrap()
    };
    let frames = vec![
        frame(Body::Holds(vec![hold("first")])),
        frame(Body::Keepalive),
        frame(Body::Holds(vec![hold("second")])),
        frame(Body::Ok),
    ];
    let Body::Holds(holds) = answered_body(frames).unwrap() else {
        panic!("the pages are not joined");
    };
    let names: Vec<_> = holds.iter().map(|hold| hold.name.as_str()).collect();
    assert_eq!(names, ["first", "second"]);
}

#[test]
fn an_answer_too_large_for_pages_arrives_whole_from_its_pieces() {
    let text = "x".repeat(shoal_protocol::protocol::MAX_FRAME);
    let frames = Response::new(1, Body::TrackedConfig(text.clone()))
        .frames()
        .unwrap();
    assert!(frames.len() > 1, "the answer fits one frame");
    let frames = (frames.into_iter())
        .map(|response| serde_json::to_value(response).unwrap())
        .collect();
    let Body::TrackedConfig(answered) = answered_body(frames).unwrap() else {
        panic!("the pieces are not joined");
    };
    assert_eq!(answered, text);
}

#[test]
fn a_listing_whose_page_came_in_pieces_arrives_whole() {
    let big = "x".repeat(shoal_protocol::protocol::MAX_FRAME);
    let pages = [
        Body::ApiResponse(vec!["small".to_owned()]),
        Body::ApiResponse(vec![big.clone()]),
        Body::ApiResponse(vec!["last".to_owned()]),
    ];
    let mut frames: Vec<serde_json::Value> = Vec::new();
    for page in pages {
        let sent = Response::new(1, page).frames().unwrap();
        frames.extend(
            sent.into_iter()
                .map(|frame| serde_json::to_value(frame).unwrap()),
        );
    }
    frames.push(frame(Body::Ok));
    let Body::ApiResponse(listed) = answered_body(frames).unwrap() else {
        panic!("the pages are not joined");
    };
    assert_eq!(listed, ["small".to_owned(), big, "last".to_owned()]);
}

#[test]
fn a_single_answer_from_another_machine_stays_as_sent() {
    assert!(matches!(answered_body(vec![frame(Body::Ok)]), Ok(Body::Ok)));
}

#[tokio::test]
async fn a_request_told_while_the_hub_cannot_be_reached_waits_in_the_outbox() {
    use shoal_protocol::{protocol::ThreadMethod, threads::CloseCause};
    let root = tempfile::tempdir().unwrap();
    let hub = crate::stand_in::hub(&root, shoal_core::config::Config::default()).await;
    let laptop: Member = serde_json::from_value(json!({
        "id": "laptop", "kind": "machine", "name": "laptop", "version": null,
        "joined_at": 1, "last_seen": null, "connected": false, "hub": false,
    }))
    .unwrap();
    let closed = Operation::Thread(ThreadMethod::CloseOpened {
        opener: SpaceId::from("writer".to_owned()),
        cause: CloseCause::Removal,
    });
    let daemon = crate::stand_in::StandIn::default();
    hub.tell_machine(&daemon, &laptop, closed).await.unwrap();
    let kept = (hub.store)
        .read(crate::telling::first_outbox)
        .await
        .unwrap();
    let Some((
        _,
        Call::Forward {
            machine,
            request,
            delivery,
        },
    )) = kept
    else {
        panic!("the outbox kept {kept:?}");
    };
    assert_eq!((machine.as_str(), delivery), ("laptop", Delivery::Queued));
    assert_eq!(
        request.method["close_opened"]["opener"], "writer",
        "{request:?}"
    );
    assert!(request.unheard, "the user hears nothing of it: {request:?}");
}

#[test]
fn an_offline_refusal_names_the_takeover_and_keeps_its_reason() {
    let id = SpaceId::from("space-seven".to_owned());
    let refused: anyhow::Error = Refusal::new(Reason::NotReady, "machine laptop is offline").into();
    assert!(!in_transit(&refused));
    let named = naming_restore(refused, &id);
    assert_eq!(Reason::of(&named), Some(Reason::NotReady));
    assert!(
        named.to_string().ends_with("shoal restore space-seven"),
        "{named}"
    );
}

#[test]
fn a_call_unanswered_on_its_way_keeps_its_type_without_a_takeover() {
    let unanswered = anyhow::Error::new(session::Unanswered("the link closed".into()));
    assert!(in_transit(&unanswered.context("forwarding")));
}
