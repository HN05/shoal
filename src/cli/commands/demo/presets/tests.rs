use super::*;
use crate::demos::Scheme;

fn launch(preset: Preset, variable: &str, value: &str) -> Launch {
    Launch {
        id: "launch".into(),
        name: "app".into(),
        preset: Some(preset),
        open: None,
        scheme: None,
        environment: BTreeMap::from([(variable.to_owned(), value.to_owned())]),
        origin: crate::sessions::Origin::created(
            crate::sessions::HostName::Zmx,
            "first-app".into(),
        ),
    }
}

#[test]
fn web_demos_print_their_url_and_over_ssh_how_to_forward_it() {
    let web = launch(Preset::Web, PORT, "49200");
    assert_eq!(
        instructions(Preset::Web, &web, None).unwrap(),
        ["http://localhost:49200"]
    );
    let over_ssh = instructions(Preset::Web, &web, Some("10.0.0.2 52000 10.0.0.1 22")).unwrap();
    assert_eq!(over_ssh[0], "http://localhost:49200");
    assert!(
        over_ssh[1].starts_with("Forward the port first: ssh -L 49200:localhost:49200 ")
            && over_ssh[1].ends_with("@10.0.0.1"),
        "{}",
        over_ssh[1]
    );
}

#[test]
fn web_demos_may_use_https() {
    let mut web = launch(Preset::Web, PORT, "49200");
    web.scheme = Some(Scheme::Https);
    assert_eq!(
        instructions(Preset::Web, &web, None).unwrap(),
        ["https://localhost:49200"]
    );
}

#[test]
fn server_demos_print_their_address_without_a_scheme() {
    let dns = launch(Preset::Server, PORT, "49300");
    assert_eq!(
        instructions(Preset::Server, &dns, None).unwrap(),
        ["localhost:49300"]
    );
    let over_ssh = instructions(Preset::Server, &dns, Some("10.0.0.2 52000 10.0.0.1 22")).unwrap();
    assert!(over_ssh[1].starts_with("Forward the port first: ssh -L 49300:localhost:49300 "));
}

#[test]
fn simulator_demos_name_their_device_over_ssh() {
    let app = launch(Preset::Simulator, SIM_UDID, "UDID-1");
    assert_eq!(
        instructions(Preset::Simulator, &app, Some("10.0.0.2 52000 10.0.0.1 22")).unwrap(),
        ["Demo app runs on simulator UDID-1; view it with Screen Sharing to 10.0.0.1"]
    );
    assert_eq!(
        instructions(Preset::Simulator, &app, None).unwrap(),
        ["Opening simulator UDID-1"]
    );
}

#[test]
fn web_demos_reserve_a_port_named_after_them_unless_ports_are_configured() {
    let chosen = |preset, ports: Option<Vec<String>>| Chosen {
        name: "site".into(),
        demo: crate::demos::Demo {
            cmd: vec!["serve".into()],
            preset,
            open: None,
            scheme: None,
            ports,
        },
        command: Vec::new(),
    };
    assert_eq!(port_names(&chosen(Some(Preset::Web), None)), ["site"]);
    assert_eq!(
        port_names(&chosen(Some(Preset::Web), Some(vec!["web".into()]))),
        ["web"]
    );
    assert_eq!(port_names(&chosen(Some(Preset::Server), None)), ["site"]);
    assert!(port_names(&chosen(None, None)).is_empty());
}
