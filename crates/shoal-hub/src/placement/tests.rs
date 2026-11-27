use shoal_protocol::hub::placement::Unanswered;
use shoal_schedule::placement::{
    Placed, Placement, Reason,
    snapshot::{Capacity, Lifetime, Machine, Request, Snapshot},
};

use super::among;

/// A start from the laptop in a repository only it has, which the server,
/// connected too, lacks.
fn snapshot() -> Snapshot {
    let machine = |id: &str| Machine {
        id: id.into(),
        name: id.into(),
        always_on: true,
        capacity: Some(Capacity {
            agents: vec!["claude".into()],
            ..Capacity::default()
        }),
    };
    Snapshot {
        request: Request {
            origin: Some("example.com/owner/repo".into()),
            base: None,
            agent: "claude".into(),
            mode: "worker".into(),
            heavy: true,
            lifetime: Lifetime::Lasting,
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

#[tokio::test(start_paused = true)]
async fn the_registered_placer_decides_under_the_external_policy() {
    use shoal_core::config::Config;
    use shoal_schedule::placement::Policy;

    use crate::stand_in::{self, StandIn};

    let root = tempfile::tempdir().unwrap();
    let config = Config::from_toml("[machine.hub]\nplacement = \"external\"\n").unwrap();
    let hub = stand_in::hub(&root, config).await;
    let own = hub.own_member().await.unwrap();
    stand_in::describe(&hub, &own, &["claude"]);
    let mut request = snapshot().request;
    request.caller = own.clone();
    let mut registration = hub.placers.register();
    let answering = async {
        let asked = registration.asked.recv().await.unwrap();
        assert!(hub.placers.answer(asked.number, on(own.as_str())));
    };
    let daemon = StandIn::default();
    let (decision, ()) = tokio::join!(hub.place_for(&daemon, request), answering);
    let decision = decision.unwrap().unwrap();
    assert_eq!(decision.machine, own);
    assert_eq!(decision.policy, Policy::External);
    assert_eq!(decision.unanswered, None);
}
