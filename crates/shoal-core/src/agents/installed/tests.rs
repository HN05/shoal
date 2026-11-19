use super::*;
use crate::{config::named_commands, test_support::install_test_script};

#[test]
fn agents_are_installed_only_when_every_program_they_start_is_on_path() {
    let bin = tempfile::tempdir().unwrap();
    for program in ["claude", "probe"] {
        let path = bin.path().join(program);
        install_test_script(&path, "#!/bin/sh\n");
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
    let installed = |name: &str| installed(&name.into(), &commands, search_path);

    assert!(installed("claude"));
    assert!(!installed("codex"));
    // Happy needs both its own CLI and the agent it runs.
    assert!(!installed("happy-claude"));
    install_test_script(bin.path().join("happy"), "#!/bin/sh\n");
    assert!(installed("happy-claude"));
    assert!(!installed("happy-codex"));

    assert!(installed("probe"));
    assert!(installed("absolute"));
    assert!(installed("relative"));
    assert!(!installed("missing"));
    assert!(!installed("undefined"));
}
