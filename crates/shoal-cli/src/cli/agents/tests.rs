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
    install_test_script(&bin.path().join("happy"), "#!/bin/sh\n");
    assert!(installed("happy-claude"));
    assert!(!installed("happy-codex"));

    assert!(installed("probe"));
    assert!(installed("absolute"));
    assert!(installed("relative"));
    assert!(!installed("missing"));
    assert!(!installed("undefined"));
}

#[test]
fn enabled_turns_an_agent_off_and_happys_agents_on() {
    let settings = |text: &str| {
        let config = crate::config::Config::from_toml(text).unwrap();
        config.resolve(&Default::default()).unwrap()
    };
    let off = settings("[agents.claude]\nenabled = false\n");
    assert!(agent_off(&off, &"claude".into()).is_some());
    assert!(agent_off(&off, &"codex".into()).is_none());
    assert!(!names(&off).contains(&"claude".into()));
    // A turned-off agent does not review either.
    assert!(off.check_reviewer(&"claude".into()).is_err());
    assert!(off.check_reviewer(&"codex".into()).is_ok());
    assert!(agent_off(&off, &"happy-codex".into()).is_some());
    let on = settings("[agents.happy]\nenabled = true\n");
    assert!(agent_off(&on, &"happy-codex".into()).is_none());
}

#[test]
fn a_reviewer_needs_only_its_review_command_installed() {
    let config = crate::config::Config::from_toml(
        "[agents.present]\ncommand = ['sh']\nreview = ['{prompt}', '{schema}']\n\
         [agents.absent]\ncommand = ['no-such-reviewer']\nreview = ['{prompt}', '{schema}']\n",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    ensure_reviewer_available(&"present".into(), &settings).unwrap();
    let refused = ensure_reviewer_available(&"absent".into(), &settings).unwrap_err();
    assert!(
        refused.to_string().contains("no-such-reviewer"),
        "{refused}"
    );
    assert!(ensure_reviewer_available(&"unknown".into(), &settings).is_err());
}
