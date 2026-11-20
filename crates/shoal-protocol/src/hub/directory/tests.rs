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
        below_elsewhere: vec![(
            shoal_ids::space_ids::SpaceId::from("here".to_owned()),
            shoal_ids::space_ids::SpaceId::from("space-1".to_owned()),
        )],
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
    assert_eq!(view.below_elsewhere[0].1.as_str(), "space-1");
}

#[test]
fn a_space_whose_threads_fill_more_than_a_frame_arrives_whole() {
    let threads: Vec<serde_json::Value> = (1..=6)
        .map(|item| {
            json!({
                "id": item, "space_number": 4, "item": item, "space_id": "remote", "space": "big",
                "kind": "question", "name": "Which?", "opener": "space",
                "holder": {"kind": "user"}, "created_at": 0,
                "posts": [{
                    "id": item, "created_at": 0, "side": "opener", "sender": "space",
                    "text": "x".repeat(16_000),
                }],
            })
        })
        .collect();
    let listed: Listed = serde_json::from_value(json!({
        "machine": "laptop", "id": "remote", "number": 4, "name": "big", "kind": "chat",
        "status": "needs-input", "reported_at": 0, "threads": threads,
    }))
    .unwrap();
    let view = HubView {
        machine: MemberId::from("desk".to_owned()),
        directory: Directory {
            machines: Vec::new(),
            spaces: vec![listed],
            conflicts: Vec::new(),
        },
        below_elsewhere: Vec::new(),
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
    assert_eq!(view.directory.spaces[0].space.threads.len(), 6);
}

fn member(id: &str, name: &str, kind: super::super::MemberKind) -> Member {
    Member {
        id: id.into(),
        kind,
        name: name.into(),
        version: None,
        joined_at: UnixTime::from_seconds(0),
        last_seen: None,
        connected: true,
        hub: false,
        availability: None,
        agents: Vec::new(),
    }
}

fn directory(members: Vec<Member>) -> Directory {
    Directory {
        machines: members,
        ..Directory::default()
    }
}

#[test]
fn a_machine_is_named_by_its_id_or_a_name_only_it_has() {
    let machine = |id, name| member(id, name, super::super::MemberKind::Machine);
    let directory = directory(vec![
        machine("m1", "laptop"),
        machine("m2", "desk"),
        machine("m3", "desk"),
    ]);
    assert_eq!(directory.named_machine("m3").unwrap().id.as_str(), "m3");
    assert_eq!(directory.named_machine("laptop").unwrap().id.as_str(), "m1");
    assert!(directory.named_machine("desk").is_err());
    assert!(directory.named_machine("server").is_err());
}

#[test]
fn a_device_s_name_never_names_a_machine() {
    use super::super::MemberKind::{Device, Machine};
    let directory = directory(vec![
        member("m1", "desk", Machine),
        member("d1", "desk", Device),
    ]);
    assert_eq!(directory.named_machine("desk").unwrap().id.as_str(), "m1");
    assert!(directory.named_machine("d1").is_err());
}
