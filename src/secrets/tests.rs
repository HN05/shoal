use super::*;

fn secret(command: &[&str]) -> Secret {
    Secret {
        command: command.iter().map(|word| word.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn names_and_commands_are_validated() {
    for (name, command) in [
        ("TOKEN", &[][..]),
        ("TOKEN", &[""][..]),
        ("TOKEN", &["printf", "a\0b"][..]),
        ("1TOKEN", &["true"][..]),
        ("SHOAL_TOKEN", &["true"][..]),
        ("PATH", &["true"][..]),
    ] {
        let secrets = Secrets::from([(name.into(), secret(command))]);
        assert!(validate(&secrets).is_err(), "{name} {command:?}");
    }
    for tool in ["", ".secrets.json", "bin/tool", "a\0b"] {
        let mut scoped = secret(&["true"]);
        scoped.tools = vec![tool.into()];
        let secrets = Secrets::from([("TOKEN".into(), scoped)]);
        assert!(validate(&secrets).is_err(), "{tool:?}");
    }
    let mut gated = secret(&["true"]);
    gated.requires_approval = true;
    assert!(validate(&Secrets::from([("TOKEN".into(), gated.clone())])).is_err());
    gated.tools = vec!["agent-browser".into()];
    assert!(validate(&Secrets::from([("TOKEN".into(), gated)])).is_ok());
}

#[tokio::test]
async fn fetch_strips_one_line_break_and_rejects_failures() {
    let value = fetch("TOKEN", &secret(&["printf", "a b\\n\\n"]), None)
        .await
        .unwrap();
    assert_eq!(value, "a b\n");
    let value = fetch("TOKEN", &secret(&["printf", "value\\r\\n"]), None)
        .await
        .unwrap();
    assert_eq!(value, "value");
    for command in [&["false"][..], &["printf", "\\n"], &["printf", "a\\0b"]] {
        assert!(
            fetch("TOKEN", &secret(command), None).await.is_err(),
            "{command:?}"
        );
    }
}

#[tokio::test]
async fn launchers_that_cannot_join_path_stop_the_launch() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path().join("a:b"));
    std::fs::create_dir_all(&paths.state).unwrap();
    let mut scoped = secret(&["true"]);
    scoped.tools = vec!["agent-browser".into()];
    let error = Launch::prepare(&Secrets::from([("TOKEN".into(), scoped)]), &paths)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot go on PATH"),
        "{error:#}"
    );
}

#[test]
fn scoped_runs_keep_their_token_to_fetch_secrets() {
    assert!(!must_authorize("tool", false, false).unwrap());
    assert!(must_authorize("tool", true, true).unwrap());
    let error = must_authorize("tool", true, false).unwrap_err();
    assert!(error.to_string().contains("SHOAL_SCOPE_TOKEN"), "{error}");
}

#[test]
fn launchers_search_only_later_path_entries() {
    let path = OsStr::new("/inner:/outer:/bin");
    assert_eq!(
        path_after(path, Path::new("/inner")).unwrap(),
        "/outer:/bin"
    );
    assert_eq!(path_after(path, Path::new("/outer")).unwrap(), "/bin");
    assert_eq!(
        path_after(path, Path::new("/elsewhere")).unwrap(),
        "/inner:/outer:/bin"
    );
}

#[tokio::test]
async fn apply_records_the_names_hooks_drop() {
    let launch = Launch {
        environment: vec![
            ("B_TOKEN".into(), "b".into()),
            ("A_TOKEN".into(), "a".into()),
        ],
        launchers: None,
    };
    let mut command = tokio::process::Command::new("true");
    launch.apply(&mut command);
    let envs: BTreeMap<_, _> = command
        .as_std()
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
        .collect();
    assert_eq!(envs[std::ffi::OsStr::new("A_TOKEN")], Some("a".into()));
    let names = envs[std::ffi::OsStr::new(crate::env::SECRET_ENV)]
        .clone()
        .unwrap();
    let names: Vec<_> = names.to_str().unwrap().split(':').collect();
    assert!(names.contains(&"A_TOKEN") && names.contains(&"B_TOKEN"));
}
