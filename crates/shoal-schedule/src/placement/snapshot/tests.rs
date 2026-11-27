use super::{AgentStart, Capacity, Demand, Exclusive, Lifetime, Registered, Request};

/// A capacity that registers each of `repositories` with its demand.
fn serving(repositories: &[(&str, Demand)]) -> Capacity {
    Capacity {
        repositories: (repositories.iter())
            .map(|(origin, demand)| Registered {
                origin: origin.to_string(),
                demand: *demand,
                ..Registered::default()
            })
            .collect(),
        ..Capacity::default()
    }
}

#[test]
fn a_machine_alone_serves_the_repositories_no_other_has_with_their_demand() {
    let busy = |busy_seconds, heavy_waiting| Demand {
        busy_seconds,
        heavy_waiting,
    };
    let none = Demand::default();
    let mac = serving(&[
        ("app", busy(600, 1)),
        ("lib", busy(60, 0)),
        ("tool", busy(5, 2)),
    ]);
    let others = [serving(&[("lib", none)]), serving(&[("web", none)])];
    let exclusive = Exclusive::among(&mac, &others);
    assert_eq!(exclusive.repositories, ["app", "tool"]);
    assert_eq!(exclusive.demand, busy(605, 3));
}

fn job() -> Request {
    Request {
        origin: Some("example.com/owner/repo".into()),
        base: None,
        agent: None,
        heavy: true,
        lifetime: Lifetime::Disposable,
        caller: "laptop".into(),
        parent: None,
    }
}

#[test]
fn an_agent_start_keeps_its_tool_and_mode_beside_the_request() {
    let start = Request {
        agent: Some(AgentStart {
            agent: "claude".into(),
            mode: "worker".into(),
        }),
        ..job()
    };
    let written = serde_json::to_value(&start).unwrap();
    assert_eq!(written["agent"], "claude");
    assert_eq!(written["mode"], "worker");
    assert_eq!(serde_json::from_value::<Request>(written).unwrap(), start);
}

#[test]
fn a_job_names_no_agent() {
    let written = serde_json::to_value(job()).unwrap();
    assert_eq!(written.get("agent"), None);
    assert_eq!(written.get("mode"), None);
    assert_eq!(serde_json::from_value::<Request>(written).unwrap(), job());
}
