use super::*;

fn message(error: Error) -> String {
    error.to_string()
}

#[test]
fn the_global_file_splits_into_machine_and_repository_settings() {
    let (machine, repository) = global(
        "agent.default = 'claude'\n[machine]\nkeep_awake = true\n[cleanup.auto]\nenabled = false\n",
    )
    .unwrap();
    assert!(machine.keep_awake);
    assert!(repository.agent.default.is_some());
    assert_eq!(repository.cleanup.auto.enabled, Some(false));
}

#[test]
fn unknown_keys_fail_with_their_position_in_either_part() {
    let error =
        message(global("terminal.focus = true\n[machine]\nkeep_awak = true\n").unwrap_err());
    assert!(error.contains("line 3, column 1"), "{error}");
    let error = message(global("[terminal.herdr]\nenabld = true\n").unwrap_err());
    assert!(error.contains("line 2, column 1"), "{error}");
    let error = message(repository("[terminal.rex]\n\nnew_windw = true\n").unwrap_err());
    assert!(error.contains("line 3, column 1"), "{error}");
}

#[test]
fn keys_in_the_wrong_file_name_the_file_they_belong_in() {
    let error =
        message(repository("terminal.focus = true\n[machine]\nkeep_awake = true\n").unwrap_err());
    assert!(error.contains("line 2"), "{error}");
    assert!(error.contains("[machine]"), "{error}");
    for (text, key) in [
        (
            "terminal.focus = true\nsetup.command = 'scripts/setup'\n",
            "setup.command",
        ),
        ("[setup.seed]\ncommand = 'scripts/seed'\n", "setup.seed"),
    ] {
        let error = message(global(text).unwrap_err());
        assert!(error.contains(key), "{error}");
        assert!(repository(text).is_ok());
    }
}

#[test]
fn removed_settings_are_refused() {
    assert!(global("[machine.bitbucket]\ntoken_command = ['security']\n").is_err());
    let text = "[agent.forge_clis]\nfj = '~/fj'\n";
    assert!(global(text).is_err());
    assert!(repository(text).is_err());
    // Settings the packaged template once wrote name their replacement.
    for (text, replacement) in [
        ("[agents.codex]\ndefault_mode = 'cli'\n", "app.default"),
        ("[terminal.happy]\nenabled = false\n", "[agents.happy]"),
    ] {
        let error = message(global(text).unwrap_err());
        assert!(error.contains(replacement), "{error}");
    }
}
