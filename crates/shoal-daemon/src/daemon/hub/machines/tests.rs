use shoal_protocol::hub::{Member, MemberKind};

use super::select;
use crate::time::UnixTime;

fn machine(id: &str, name: &str) -> Member {
    member(id, name, MemberKind::Machine)
}

fn member(id: &str, name: &str, kind: MemberKind) -> Member {
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

#[test]
fn a_machine_is_named_by_its_id_or_a_name_only_it_has() {
    let machines = || {
        vec![
            machine("m1", "laptop"),
            machine("m2", "desk"),
            machine("m3", "desk"),
        ]
    };
    assert_eq!(select(machines(), "m3").unwrap().id.as_str(), "m3");
    assert_eq!(select(machines(), "laptop").unwrap().id.as_str(), "m1");
    assert!(select(machines(), "desk").is_err());
    assert!(select(machines(), "server").is_err());
}

#[test]
fn a_device_s_name_never_names_a_machine() {
    let members = vec![
        machine("m1", "desk"),
        member("d1", "desk", MemberKind::Device),
    ];
    assert_eq!(select(members.clone(), "desk").unwrap().id.as_str(), "m1");
    assert!(select(members, "d1").is_err());
}
