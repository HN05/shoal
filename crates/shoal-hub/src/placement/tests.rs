use shoal_protocol::hub::placement::Unanswered;
use shoal_schedule::placement::{
    Placed, Placement, Reason,
    snapshot::{Capacity, Machine, Request, Snapshot},
};

use super::among;

/// A start from the laptop in a repository only it has, which the server,
/// connected too, lacks.
fn snapshot() -> Snapshot {
    let machine = |id: &str| Machine {
        id: id.into(),
        name: id.into(),
        always_on: true,
        agents: vec!["claude".into()],
        capacity: Some(Capacity::default()),
    };
    Snapshot {
        request: Request {
            origin: Some("example.com/owner/repo".into()),
            base: None,
            agent: "claude".into(),
            mode: "worker".into(),
            heavy: true,
            caller: "laptop".into(),
            parent: None,
        },
        machines: vec![machine("laptop"), machine("server")],
    }
}

fn on(machine: &str) -> Placement {
    Placement {
        ranked: vec![Placed {
            machine: machine.into(),
            reason: Reason::Local,
        }],
        skipped: Vec::new(),
    }
}

#[test]
fn a_placer_chooses_only_a_connected_machine_the_start_can_go_to() {
    let snapshot = snapshot();
    assert_eq!(among(on("laptop"), &snapshot), Ok(on("laptop")));
    // The server would have to clone the repository.
    assert_eq!(among(on("server"), &snapshot), Err(Unanswered::Failed));
    assert_eq!(among(on("gone"), &snapshot), Err(Unanswered::Failed));
    assert_eq!(
        among(Placement::default(), &snapshot),
        Err(Unanswered::Failed)
    );
}
