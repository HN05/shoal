use super::*;
use shoal_protocol::demos::{PORT, SIM_UDID, Scheme};
use shoal_protocol::sessions::{HostName, Origin};
use std::collections::BTreeMap;

fn launch(preset: Preset, variable: &str, value: &str) -> Launch {
    Launch {
        id: "launch".into(),
        name: "app".into(),
        preset: Some(preset),
        open: None,
        scheme: None,
        viewer_command: None,
        environment: BTreeMap::from([(variable.to_owned(), value.to_owned())]),
        origin: Origin::created(HostName::Zmx, "first-app".into()),
    }
}

/// Where the announcement says the demo listens, after checking it names
/// the demo.
fn announced_location(launch: &Launch) -> String {
    let line = announcement(launch).expect("a serving demo is announced");
    assert!(line.contains(&launch.name), "{line}");
    line.rsplit(' ').next().unwrap().to_owned()
}

#[test]
fn serving_demos_announce_where_they_listen() {
    let server = launch(Preset::Server, PORT, "49200");
    assert_eq!(announced_location(&server), "localhost:49200");
    let web = launch(Preset::Web, PORT, "49200");
    assert_eq!(announced_location(&web), "http://localhost:49200");
    let https = Launch {
        scheme: Some(Scheme::Https),
        ..web
    };
    assert_eq!(announced_location(&https), "https://localhost:49200");
}

#[test]
fn demos_opened_another_way_announce_nothing() {
    let opened = Launch {
        open: Some(vec!["open-app".into()]),
        ..launch(Preset::Web, PORT, "49200")
    };
    assert_eq!(announcement(&opened), None);
    assert_eq!(
        announcement(&launch(Preset::Simulator, SIM_UDID, "UDID")),
        None
    );
}

#[test]
fn quick_exits_back_off_and_a_settled_run_restarts_at_once() {
    let mut restarts = Restarts::default();
    let quick = Duration::from_secs(1);
    let delays: Vec<_> = (0..7).map(|_| restarts.after(quick).as_secs()).collect();
    assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
    assert_eq!(restarts.after(SETTLED), Duration::ZERO);
    assert_eq!(restarts.after(quick), FIRST_DELAY);
}
