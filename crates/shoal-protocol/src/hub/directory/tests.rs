use serde_json::json;

use super::*;
use crate::protocol::{Body, MAX_FRAME, Response};

#[test]
fn a_view_larger_than_a_frame_arrives_in_pages_whole() {
    let spaces: Vec<Listed> = (0..2_000)
        .map(|index| {
            serde_json::from_value(json!({
                "machine": "laptop", "id": format!("space-{index}"), "number": index,
                "name": format!("space {index}"), "kind": "workspace", "status": "working",
                "reported_at": 0,
            }))
            .unwrap()
        })
        .collect();
    let view = HubView {
        machine: MemberId::from("desk".to_owned()),
        directory: Directory {
            machines: Vec::new(),
            spaces,
            conflicts: Vec::new(),
        },
    };
    let frames = Response::new(1, Body::HubView(view.into_parts()))
        .frames()
        .unwrap();
    assert!(frames.len() > 1);
    let mut frames = frames.into_iter();
    let mut body = frames.next().unwrap().body;
    for frame in frames {
        assert!(serde_json::to_vec(&frame).unwrap().len() <= MAX_FRAME);
        body.extend(frame.body).unwrap();
    }
    let Body::HubView(parts) = body else {
        panic!("expected a hub view");
    };
    let view = HubView::from_parts(parts).unwrap().unwrap();
    assert_eq!(view.machine.as_str(), "desk");
    assert_eq!(view.directory.spaces.len(), 2_000);
    assert_eq!(view.directory.spaces[1_999].space.id.as_str(), "space-1999");
}

#[test]
fn a_space_whose_questions_fill_more_than_a_frame_arrives_whole() {
    let questions: Vec<serde_json::Value> = (1..=6)
        .map(|item| {
            json!({
                "space_number": 4, "item": item, "space_id": "remote", "space": "big",
                "created_at": 0, "question": "x".repeat(12_000),
                "recommendation": "y".repeat(4_000), "status": "open",
            })
        })
        .collect();
    let listed: Listed = serde_json::from_value(json!({
        "machine": "laptop", "id": "remote", "number": 4, "name": "big", "kind": "chat",
        "status": "needs-input", "reported_at": 0, "questions": questions,
    }))
    .unwrap();
    let view = HubView {
        machine: MemberId::from("desk".to_owned()),
        directory: Directory {
            machines: Vec::new(),
            spaces: vec![listed],
            conflicts: Vec::new(),
        },
    };
    let mut frames = Response::new(1, Body::HubView(view.into_parts()))
        .frames()
        .unwrap()
        .into_iter();
    let mut body = frames.next().unwrap().body;
    for frame in frames {
        body.extend(frame.body).unwrap();
    }
    let Body::HubView(parts) = body else {
        panic!("expected a hub view");
    };
    let view = HubView::from_parts(parts).unwrap().unwrap();
    assert_eq!(view.directory.spaces.len(), 1);
    assert_eq!(view.directory.spaces[0].space.questions.len(), 6);
}
