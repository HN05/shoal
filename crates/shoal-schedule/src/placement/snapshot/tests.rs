use super::{Capacity, Demand, Exclusive, Registered};

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
