use super::*;

#[test]
fn launches_name_the_missing_program_or_undefined_agent() {
    let settings = crate::config::Config::from_toml(
        "[agents.probe]\ncommand = [\"shoal-test-missing-program\"]\n",
    )
    .unwrap()
    .resolve(&crate::config::repo::ConfigLayers::default())
    .unwrap();
    let error = ensure_installed("probe", &settings).unwrap_err();
    assert!(
        error.to_string().contains("shoal-test-missing-program"),
        "{error}"
    );
    assert!(ensure_installed("undefined", &settings).is_err());
}

#[cfg(feature = "happy")]
#[test]
fn an_app_run_agent_takes_the_model_of_the_agent_it_starts() {
    let config = crate::config::Config::from_toml(
        "[agent]\ndefault = 'happy-claude'\neffort = 'xhigh'\n[agents.claude]\nmodel = 'opus'\neffort = 'low'\n",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    let choice = |model: Option<&str>, effort: &str| ModelChoice {
        model: model.map(str::to_owned),
        effort: Some(effort.to_owned()),
    };
    // `[agent]` chose happy-claude, so its effort applies over Claude's own.
    assert_eq!(
        resolve_model(&settings, "happy-claude", Default::default()),
        choice(Some("opus"), "xhigh")
    );
    assert_eq!(
        resolve_model(&settings, "happy-codex", Default::default()),
        ModelChoice::default()
    );
    let requested = ModelChoice {
        model: None,
        effort: Some("high".into()),
    };
    assert_eq!(
        resolve_model(&settings, "happy-claude", requested),
        choice(Some("opus"), "high")
    );
}
