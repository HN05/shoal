use super::*;

#[test]
fn agents_default_to_json_unless_their_launcher_carries_a_setting() {
    for setting in [None, Some("")] {
        assert_eq!(
            agent_output_under(setting.map(OsStr::new)),
            Some((OUTPUT, "json"))
        );
    }
    for setting in ["text", "json", "invalid"] {
        assert_eq!(agent_output_under(Some(OsStr::new(setting))), None);
    }
}

fn path_of(command: &Command) -> Option<OsString> {
    command
        .as_std()
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value.map(OsStr::to_owned))
}

#[test]
fn preset_directories_go_first_on_the_path_a_process_gets() {
    let mut command = Command::new("true");
    command.env("PATH", "/usr/bin:/bin");
    let values = BTreeMap::from([
        (PATH_PREFIX.to_owned(), OsString::from("/cache/xcode/bin")),
        ("GOFLAGS".to_owned(), OsString::from("-trimpath")),
    ]);
    apply_configured(&mut command, &values);
    assert_eq!(path_of(&command).unwrap(), "/cache/xcode/bin:/usr/bin:/bin");
    assert!(
        command
            .as_std()
            .get_envs()
            .all(|(name, _)| name != PATH_PREFIX)
    );
    // A later step, such as the agent account's wrappers, goes before it.
    prepend_path(&mut command, OsStr::new("/wrappers"));
    assert_eq!(
        path_of(&command).unwrap(),
        "/wrappers:/cache/xcode/bin:/usr/bin:/bin"
    );

    let exported = export_configured(
        BTreeMap::from([(PATH_PREFIX.to_owned(), "/cache/xcode/bin".to_owned())]),
        Some("/usr/bin".into()),
    )
    .unwrap();
    assert_eq!(
        exported,
        BTreeMap::from([("PATH".to_owned(), "/cache/xcode/bin:/usr/bin".to_owned())])
    );
    // A PATH `[env]` sets is the one the directories go before.
    let exported = export_configured(
        BTreeMap::from([
            (PATH_PREFIX.to_owned(), "/cache/xcode/bin".to_owned()),
            ("PATH".to_owned(), "/tools".to_owned()),
        ]),
        Some("/usr/bin".into()),
    )
    .unwrap();
    assert_eq!(exported["PATH"], "/cache/xcode/bin:/tools");
}

#[test]
fn configured_values_render_identity_and_only_reserved_ports_once() {
    let workspace = Workspace::new_record(
        "repo".into(),
        "task-{branch}".into(),
        PathBuf::from("/tmp/{repo}/$(false) 日本語"),
        "feature/session".into(),
        crate::state::WorkspaceState::Ready,
    );
    let ports = vec![PortReservation {
        workspace_id: workspace.id.clone(),
        name: "web".into(),
        port: 3000,
        env_var: "WEB_PORT".into(),
        reason: None,
    }];
    let values = BTreeMap::from([
        ("SESSION".into(), "{repo}:{workspace}:{branch}".into()),
        ("PROFILE".into(), "{workspace_path}/profile:{path}".into()),
        (
            "URL".into(),
            "http://localhost:{port.web}/{port.missing}".into(),
        ),
        ("GOCACHE".into(), "{cache.go}:{cache.missing}".into()),
        (
            "LITERAL".into(),
            "$HOME $(false) {unknown} {SESSION}".into(),
        ),
        ("EMPTY".into(), "".into()),
    ]);
    let caches = BTreeMap::from([("go".into(), PathBuf::from("/tmp/.caches/{cache.go}"))]);
    let rendered =
        render_configured_environment(&values, &workspace, "project-{workspace}", &ports, &caches);
    assert_eq!(
        rendered["SESSION"],
        "project-{workspace}:task-{branch}:feature/session"
    );
    assert_eq!(
        rendered["PROFILE"],
        "/tmp/{repo}/$(false) 日本語/profile:/tmp/{repo}/$(false) 日本語"
    );
    assert_eq!(rendered["URL"], "http://localhost:3000/{port.missing}");
    assert_eq!(
        rendered["GOCACHE"],
        "/tmp/.caches/{cache.go}:{cache.missing}"
    );
    assert_eq!(rendered["LITERAL"], "$HOME $(false) {unknown} {SESSION}");
    assert_eq!(rendered["EMPTY"], "");
}

#[test]
fn configured_paths_preserve_non_utf8_bytes() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let workspace = Workspace::new_record(
        "repo".into(),
        "worker".into(),
        PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec())),
        "worker".into(),
        crate::state::WorkspaceState::Ready,
    );
    let values = BTreeMap::from([("PROFILE".into(), "{workspace_path}/profile".into())]);
    let rendered =
        render_configured_environment(&values, &workspace, "repo", &[], &BTreeMap::new());
    assert_eq!(
        rendered["PROFILE"].as_os_str(),
        OsStr::from_bytes(b"/tmp/\xff/profile")
    );
}
