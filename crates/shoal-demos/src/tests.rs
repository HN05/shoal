use std::collections::BTreeMap;

use shoal_protocol::demos::{Launch, PORT, Preset, Scheme};
use shoal_protocol::space_names::SpaceSelector;

use crate::stand_in::Fixture;

fn launch(id: &str, name: &str) -> Launch {
    Launch {
        id: id.into(),
        name: name.into(),
        preset: None,
        open: None,
        scheme: None,
        viewer_command: None,
        environment: BTreeMap::new(),
        origin: shoal_core::sessions::Origin::created(
            shoal_core::sessions::HostName::Zmx,
            format!("session-{name}"),
        ),
    }
}

#[tokio::test]
async fn running_web_demos_list_their_url() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("first").await;
    let mut site = launch("launch", "site");
    site.preset = Some(Preset::Web);
    site.scheme = Some(Scheme::Https);
    site.environment = BTreeMap::from([(PORT.into(), "49200".into())]);
    let mut dns = launch("dns", "dns");
    dns.preset = Some(Preset::Server);
    dns.environment = site.environment.clone();
    for demo in [site, dns] {
        fixture.demo(&workspace.id, demo).await;
    }
    let selector = SpaceSelector::id(&workspace.id);
    let listed = fixture.demos.list(&*fixture.daemon, Some(&selector)).await;
    let urls: Vec<_> = (listed.unwrap().into_iter())
        .map(|demo| (demo.launch.name, demo.url))
        .collect();
    assert_eq!(
        urls,
        [
            ("dns".to_owned(), None),
            (
                "site".to_owned(),
                Some("https://localhost:49200".to_owned())
            )
        ]
    );
}

#[tokio::test]
async fn stopping_every_demo_stops_those_of_every_workspace() {
    let fixture = Fixture::new().await;
    let (first, second) = (
        fixture.workspace("first").await,
        fixture.workspace("second").await,
    );
    let site = fixture.demo(&first.id, launch("site", "site")).await;
    let tui = fixture.demo(&second.id, launch("tui", "tui")).await;
    (fixture.demos)
        .stop_all(&*fixture.daemon, "paused".into())
        .await
        .unwrap();
    let mut asked = fixture.daemon.asked_to_stop.lock().unwrap().clone();
    asked.sort();
    let mut running = vec![site.execution_id, tui.execution_id];
    running.sort();
    assert_eq!(asked, running);
}
