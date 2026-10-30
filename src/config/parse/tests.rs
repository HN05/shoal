use super::*;

fn message(error: Error) -> String {
    error.to_string()
}

#[test]
fn the_global_file_splits_into_machine_and_repository_settings() {
    let (machine, repository) = global(
        "default_agent = 'claude'\n[machine]\nkeep_awake = true\n[cleanup.auto]\nenabled = false\n",
    )
    .unwrap();
    assert!(machine.keep_awake);
    assert!(repository.default_agent.is_some());
    assert_eq!(repository.cleanup.auto.enabled, Some(false));
}

#[test]
fn unknown_keys_fail_with_their_position_in_either_part() {
    let error = message(global("focus = true\n[machine]\nkeep_awak = true\n").unwrap_err());
    assert!(error.contains("line 3, column 1"), "{error}");
    assert!(error.contains("unknown field `keep_awak`"), "{error}");
    let error = message(global("[herdr]\nenabld = true\n").unwrap_err());
    assert!(error.contains("line 2, column 1"), "{error}");
    let error = message(repository("[rex]\n\nnew_windw = true\n").unwrap_err());
    assert!(error.contains("line 3, column 1"), "{error}");
}

#[test]
fn keys_in_the_wrong_file_name_the_file_they_belong_in() {
    let error = message(repository("focus = true\n[machine]\nkeep_awake = true\n").unwrap_err());
    assert!(error.contains("line 2"), "{error}");
    assert!(
        error.contains("[machine] belongs in the global config"),
        "{error}"
    );
    for (text, key) in [
        ("focus = true\nsetup_cmd = 'scripts/setup'\n", "setup_cmd"),
        ("[seed]\ncmd = 'scripts/seed'\n", "seed"),
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
fn every_repository_only_key_is_one_the_global_file_refuses() {
    let mut keys = Vec::new();
    RepoConfig::repository_only("", &mut keys);
    assert_eq!(keys, ["setup_cmd", "seed"]);
}
