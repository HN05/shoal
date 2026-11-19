use super::*;
use shoal_protocol::demos::Scheme;

fn launch(preset: Preset, variable: &str, value: &str) -> Launch {
    Launch {
        id: "launch".into(),
        name: "app".into(),
        preset: Some(preset),
        open: None,
        scheme: None,
        viewer_command: None,
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
        instructions(Preset::Web, &web, None, None).unwrap(),
        ["http://localhost:49200"]
    );
    let over_ssh =
        instructions(Preset::Web, &web, Some("10.0.0.2 52000 10.0.0.1 22"), None).unwrap();
    assert_eq!(over_ssh[0], "http://localhost:49200");
    assert!(
        over_ssh[1].contains("ssh -L 49200:localhost:49200 ") && over_ssh[1].ends_with("@10.0.0.1"),
        "{}",
        over_ssh[1]
    );
}

#[test]
fn web_demos_may_use_https() {
    let mut web = launch(Preset::Web, PORT, "49200");
    web.scheme = Some(Scheme::Https);
    assert_eq!(
        instructions(Preset::Web, &web, None, None).unwrap(),
        ["https://localhost:49200"]
    );
}

#[test]
fn server_demos_print_their_address_without_a_scheme() {
    let dns = launch(Preset::Server, PORT, "49300");
    assert_eq!(
        instructions(Preset::Server, &dns, None, None).unwrap(),
        ["localhost:49300"]
    );
    let over_ssh = instructions(
        Preset::Server,
        &dns,
        Some("10.0.0.2 52000 10.0.0.1 22"),
        None,
    )
    .unwrap();
    assert!(over_ssh[1].contains("ssh -L 49300:localhost:49300 "));
}

#[test]
fn simulator_demos_print_their_viewer_s_url_over_ssh_and_how_to_forward_it() {
    let ssh = Some("10.0.0.2 52000 10.0.0.1 22");
    let app = Launch {
        viewer_command: Some(vec!["serve-sim".into(), "{udid}".into()]),
        ..launch(Preset::Simulator, SIM_UDID, "UDID-1")
    };
    let over_ssh = instructions(Preset::Simulator, &app, ssh, Some(49400)).unwrap();
    assert_eq!(over_ssh[0], "http://localhost:49400");
    assert!(
        over_ssh[1].contains("ssh -L 49400:localhost:49400 ") && over_ssh[1].ends_with("@10.0.0.1"),
        "{}",
        over_ssh[1]
    );
    let local = instructions(Preset::Simulator, &app, None, None).unwrap();
    assert!(
        matches!(local.as_slice(), [line] if line.contains("UDID-1")),
        "{local:?}"
    );
}

#[test]
fn simulator_demos_without_a_viewer_name_their_device_over_ssh() {
    let app = launch(Preset::Simulator, SIM_UDID, "UDID-1");
    let ssh = Some("10.0.0.2 52000 10.0.0.1 22");
    let over_ssh = instructions(Preset::Simulator, &app, ssh, None).unwrap();
    assert!(
        matches!(over_ssh.as_slice(), [line] if line.contains("UDID-1")),
        "{over_ssh:?}"
    );
}

#[test]
fn web_demos_reserve_a_port_named_after_them_unless_ports_are_configured() {
    let chosen = |preset, ports: Option<Vec<String>>| Chosen {
        name: "site".into(),
        demo: shoal_protocol::demos::Demo {
            cmd: vec!["serve".into()],
            preset,
            open: None,
            scheme: None,
            viewer: None,
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
