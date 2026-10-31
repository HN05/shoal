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
    assert!(error.contains("unknown field `keep_awak`"), "{error}");
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
    assert!(
        error.contains("[machine] belongs in the global config"),
        "{error}"
    );
    for (text, key) in [
        (
            "terminal.focus = true\nsetup.command = 'scripts/setup'\n",
            "setup.command",
        ),
        ("[setup.seed]\ncommand = 'scripts/seed'\n", "setup.seed"),
    ] {
        let error = message(global(text).unwrap_err());
        assert!(
            error.contains(&format!("{key} belongs in the repository config")),
            "{error}"
        );
        assert!(error.contains("TOML parse error at line"), "{error}");
        assert!(repository(text).is_ok());
    }
}

#[test]
fn removed_settings_name_what_replaces_them() {
    let error = message(global("[machine.bitbucket]\ntoken_command = ['security']\n").unwrap_err());
    assert!(error.contains("line 1"), "{error}");
    assert!(
        error.contains("set token and email under [machine.forges.\"bitbucket.org\"]"),
        "{error}"
    );
    let text = "[agent.forge_clis]\nfj_home = '~/fj'\n";
    for error in [global(text).err(), repository(text).err()] {
        let error = message(error.unwrap());
        assert!(error.contains("set agent_token under"), "{error}");
    }
}

#[test]
fn every_repository_only_key_is_one_the_global_file_refuses() {
    let mut keys = Vec::new();
    RepoConfig::repository_only("", &mut keys);
    assert_eq!(keys, ["setup.command", "setup.seed"]);
}
