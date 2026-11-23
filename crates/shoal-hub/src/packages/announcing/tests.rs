use super::*;

fn id(id: &str) -> SpaceId {
    SpaceId::from(id.to_owned())
}

#[test]
fn a_machine_that_keeps_packages_current_names_those_that_lag() {
    let behind = UnixTime::from_seconds(100);
    let unsent = HashMap::from([(id("fix"), behind)]);
    let ids = [id("fix"), id("plan")];
    let lagging = Lagging {
        space: id("fix"),
        since: Some(behind),
    };
    assert_eq!(lags(ids.iter(), Some(&unsent)), [lagging]);
}

#[test]
fn a_machine_that_keeps_none_current_names_every_space_by_an_unknown_lag() {
    let ids = [id("fix"), id("plan")];
    let unknown = |space| Lagging {
        space: id(space),
        since: None,
    };
    assert_eq!(lags(ids.iter(), None), [unknown("fix"), unknown("plan")]);
}
