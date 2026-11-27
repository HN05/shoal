use super::{Local, Placer, Reason, SkippedBy};
use crate::placement::snapshot::{Capacity, Lifetime, Machine, Request, Snapshot};

/// A chat start on the hub that launches `agents`.
fn snapshot(agents: &[&str]) -> Snapshot {
    Snapshot {
        request: Request {
            origin: None,
            base: None,
            agent: "claude".into(),
            mode: "chat".into(),
            heavy: false,
            lifetime: Lifetime::Lasting,
            caller: "hub".into(),
            parent: None,
        },
        machines: vec![Machine {
            id: "hub".into(),
            name: "hub".into(),
            always_on: true,
            capacity: Some(Capacity {
                agents: agents.iter().map(|agent| (*agent).into()).collect(),
                ..Capacity::default()
            }),
        }],
    }
}

#[test]
fn local_keeps_a_start_on_its_caller_only_where_the_caller_can_launch_it() {
    let placed = Local.place(&snapshot(&["claude"]));
    assert_eq!(
        placed.chosen().map(|placed| placed.reason),
        Some(Reason::Local)
    );
    let skipped = Local.place(&snapshot(&["codex"]));
    assert_eq!(skipped.chosen(), None);
    assert_eq!(skipped.skipped[0].why, SkippedBy::NoAgent);
}
