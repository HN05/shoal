use super::*;

fn demo(cmd: &[&str]) -> Demo {
    Demo {
        cmd: cmd.iter().map(|arg| (*arg).to_owned()).collect(),
        preset: None,
        open: None,
        scheme: None,
        viewer: None,
        ports: None,
    }
}

#[test]
fn names_follow_port_names_and_leave_subcommands_alone() {
    assert!(validate_name("web").is_ok());
    assert!(validate_name("Web").is_err());
    assert!(validate_name("ls").is_err());
    assert!(validate_name("start").is_err());
}

#[test]
fn commands_need_an_executable() {
    assert!(demo(&["npm", "run", "dev"]).validate().is_ok());
    assert!(demo(&[]).validate().is_err());
    assert!(demo(&[" "]).validate().is_err());
    let mut open = demo(&["server"]);
    open.open = Some(Vec::new());
    assert!(open.validate().is_err());
    let mut ports = demo(&["server"]);
    ports.ports = Some(vec!["Web".into()]);
    assert!(ports.validate().is_err());
    let mut https = demo(&["server"]);
    https.scheme = Some(Scheme::Https);
    assert!(https.validate().is_err());
    https.preset = Some(Preset::Web);
    assert!(https.validate().is_ok());
}

#[test]
fn a_viewer_belongs_to_a_simulator_demo() {
    let viewer: Demo = toml::from_str(
        r#"
        cmd = ["scripts/run-app.sh"]
        viewer = ["serve-sim", "--port", "{port}", "{udid}", "--codec", "mjpeg"]
        "#,
    )
    .expect("a viewer command");
    assert!(viewer.validate().is_err());
    let simulator = Demo {
        preset: Some(Preset::Simulator),
        ..viewer
    };
    assert!(simulator.validate().is_ok());
    let empty = Demo {
        viewer: Some(ViewerSetting::Command(Vec::new())),
        ..simulator
    };
    assert!(empty.validate().is_err());
}

#[test]
fn a_simulator_demo_runs_the_default_viewer_unless_its_own_or_none() {
    let simulator = |viewer: &str| -> Demo {
        toml::from_str(&format!(
            "cmd = [\"app\"]\npreset = \"simulator\"\n{viewer}"
        ))
        .expect("a simulator demo")
    };
    let default: Vec<String> = DEFAULT_VIEWER.iter().map(|arg| (*arg).to_owned()).collect();
    assert_eq!(simulator("").viewer_command(), Some(default.clone()));
    assert_eq!(simulator("viewer = true").viewer_command(), Some(default));
    assert_eq!(simulator("viewer = false").viewer_command(), None);
    assert_eq!(
        simulator(r#"viewer = ["view", "{udid}"]"#).viewer_command(),
        Some(vec!["view".to_owned(), "{udid}".to_owned()])
    );
    let web = Demo {
        preset: Some(Preset::Web),
        ..demo(&["server"])
    };
    assert_eq!(web.viewer_command(), None);
}

#[test]
fn a_listed_simulator_demo_keeps_its_viewer_and_its_command_apart() {
    let launch = Launch {
        id: "launch".into(),
        name: "app".into(),
        preset: Some(Preset::Simulator),
        open: None,
        scheme: None,
        viewer_command: Some(vec!["serve-sim".into(), "{udid}".into()]),
        environment: BTreeMap::from([(SIM_UDID.to_owned(), "UDID-1".to_owned())]),
        origin: crate::sessions::Origin::created(crate::sessions::HostName::Zmx, "app".into()),
    };
    let running = Running {
        workspace_id: "workspace".into(),
        workspace: "tried".into(),
        execution_id: "execution".into(),
        started_at: crate::time::UnixTime::from_seconds(0),
        has_run: true,
        kind: Kind::Simulator,
        url: None,
        viewer: launch.viewer(Some(49400)),
        launch,
        machine: None,
    };
    let json = serde_json::to_string(&running).unwrap();
    let read: Running = serde_json::from_str(&json).unwrap();
    assert_eq!(read, running);
    let viewer = read.viewer.unwrap();
    assert_eq!(viewer.tool, "serve-sim");
    assert_eq!(viewer.url.as_deref(), Some("http://localhost:49400"));
}
