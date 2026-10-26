use std::{fs, os::unix::fs::PermissionsExt};

use super::*;
use crate::config::named_commands;

#[test]
fn agents_are_installed_only_when_every_program_they_start_is_on_path() {
    let bin = tempfile::tempdir().unwrap();
    for program in ["claude", "probe"] {
        let path = bin.path().join(program);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut commands = named_commands::defaults();
    for (name, program) in [
        ("probe", "probe".to_owned()),
        ("missing", "missing".to_owned()),
        ("absolute", bin.path().join("probe").display().to_string()),
        ("relative", "./scripts/agent".to_owned()),
    ] {
        commands.insert(name.into(), vec![program]);
    }
    let search_path = bin.path().as_os_str();
    let installed = |name: &str| installed(name, &commands, search_path);

    assert!(installed("claude"));
    assert!(!installed("codex"));
    // Happy needs both its own CLI and the agent it runs.
    #[cfg(feature = "happy")]
    {
        assert!(!installed("happy-claude"));
        fs::copy(bin.path().join("claude"), bin.path().join("happy")).unwrap();
        assert!(installed("happy-claude"));
        assert!(!installed("happy-codex"));
    }

    assert!(installed("probe"));
    assert!(installed("absolute"));
    assert!(installed("relative"));
    assert!(!installed("missing"));
    assert!(!installed("undefined"));
}

#[test]
fn launches_name_the_missing_program_or_undefined_agent() {
    let mut commands = named_commands::defaults();
    commands.insert("probe".into(), vec!["shoal-test-missing-program".into()]);
    let error = ensure_installed("probe", &commands).unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot start probe: shoal-test-missing-program is not installed or not on PATH"
    );
    let error = ensure_installed("undefined", &commands).unwrap_err();
    assert!(error.to_string().starts_with("unknown agent"), "{error}");
}

#[test]
fn integrations_serve_only_the_names_they_list() {
    assert!(integration(IntegrationKind::Claude).trust_file().is_some());
    assert!(integration(IntegrationKind::Codex).wakes_by_prompt());
    assert!(!integration(IntegrationKind::Claude).wakes_by_prompt());
    let configured = integration(IntegrationKind::None);
    assert!(configured.trust_file().is_none());
    assert!(configured.hook_args().unwrap().is_empty());
    assert_eq!(
        builtin_names()[..2],
        ["codex".to_owned(), "claude".to_owned()]
    );
}
