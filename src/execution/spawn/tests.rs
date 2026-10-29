use std::collections::BTreeMap;

use super::*;

fn proxy(upstream: &str, key: Option<&str>) -> crate::proxy::Proxy {
    let key = key.map_or(String::new(), |env| format!("key = {{ env = '{env}' }}\n"));
    toml::from_str(&format!("upstream = '{upstream}'\n{key}")).unwrap()
}

/// A Claude launch with no proxy running yet.
fn launch(home: &std::path::Path) -> AgentLaunch {
    let paths = Paths::for_test(home);
    AgentLaunch {
        account: crate::agent_auth::Config::default()
            .prepare(&paths, &Default::default())
            .unwrap(),
        secrets: Default::default(),
        output: None,
        integration: Some(crate::ai::IntegrationKind::Claude),
        environment: Default::default(),
        home: home.to_owned(),
        routed: None,
        fetched_key: None,
        proxy: None,
        shown: None,
    }
}

#[tokio::test]
async fn a_restored_agent_runs_through_the_proxy_its_restore_command_names() {
    let home = tempfile::tempdir().unwrap();
    let mut launch = launch(home.path());
    let environment = BTreeMap::from([("PROXY_KEY".to_owned(), "sk-test".into())]);
    // A proxy enabled since the launch starts with the restore.
    let first = proxy("http://127.0.0.1:9", None);
    assert!(
        launch
            .restart_proxy(Some(first.clone()), &environment)
            .await
            .unwrap()
    );
    let url = launch.proxy.as_ref().unwrap().url.clone();
    // The same proxy keeps running.
    assert!(
        !launch
            .restart_proxy(Some(first), &environment)
            .await
            .unwrap()
    );
    assert_eq!(launch.proxy.as_ref().unwrap().url, url);
    // Another upstream or key replaces it, reading the key as a launch does.
    let moved = proxy("http://127.0.0.1:10", Some("PROXY_KEY"));
    assert!(
        launch
            .restart_proxy(Some(moved.clone()), &environment)
            .await
            .unwrap()
    );
    assert_eq!(launch.routed, Some(moved));
    assert!(launch.proxy.is_some());
    let missing = proxy("http://127.0.0.1:10", Some("MISSING_PROXY_KEY"));
    let error = launch
        .restart_proxy(Some(missing), &environment)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("MISSING_PROXY_KEY"),
        "{error:#}"
    );
    // Removing it stops it, which changes it too.
    assert!(launch.restart_proxy(None, &environment).await.unwrap());
    assert!(launch.proxy.is_none());
    assert!(!launch.restart_proxy(None, &environment).await.unwrap());
}

#[tokio::test]
async fn a_proxy_uses_the_key_its_command_printed_before_the_launch_registered() {
    let home = tempfile::tempdir().unwrap();
    let mut launch = launch(home.path());
    let fetched: crate::proxy::Proxy =
        toml::from_str("upstream = 'http://127.0.0.1:9'\nkey = { cmd = ['echo', 'sk-cmd'] }")
            .unwrap();
    launch.fetched_key = fetch_command_key(Some(&fetched)).await.unwrap();
    let key = fetched.key.clone().unwrap();
    assert_eq!(launch.fetched_key, Some((key.clone(), "sk-cmd".to_owned())));
    // The same command is not run again, so one that would fail never does.
    let failing: crate::proxy::Proxy =
        toml::from_str("upstream = 'http://127.0.0.1:9'\nkey = { cmd = ['false'] }").unwrap();
    launch.fetched_key = Some((failing.key.clone().unwrap(), "sk-fetched".to_owned()));
    assert!(
        launch
            .restart_proxy(Some(failing), &BTreeMap::new())
            .await
            .unwrap()
    );
    assert!(launch.proxy.is_some());
    // A key from the environment is read when the proxy starts.
    assert_eq!(
        fetch_command_key(Some(&proxy("http://host", Some("KEY"))))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn a_restore_that_runs_a_key_command_stops_it_at_the_limit() {
    let home = tempfile::tempdir().unwrap();
    let mut launch = launch(home.path());
    let hanging: crate::proxy::Proxy =
        toml::from_str("upstream = 'http://127.0.0.1:9'\nkey = { cmd = ['sleep', '86400'] }")
            .unwrap();
    let error = launch
        .restart_proxy(Some(hanging), &BTreeMap::new())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("did not finish within 60s"),
        "{error:#}"
    );
    assert!(launch.proxy.is_none());
}

#[tokio::test]
async fn a_claude_agent_through_its_proxy_gets_the_hour_long_prompt_cache_unless_it_sets_one() {
    let home = tempfile::tempdir().unwrap();
    let mut launch = launch(home.path());
    let unset = BTreeMap::new();
    assert_eq!(prompt_cache_1h(&launch, &unset), None);
    launch
        .restart_proxy(Some(proxy("http://127.0.0.1:9", None)), &unset)
        .await
        .unwrap();
    assert_eq!(prompt_cache_1h(&launch, &unset), Some(Some("1".into())));
    // The workspace's own value reaches the agent unchanged.
    let chosen = BTreeMap::from([("ENABLE_PROMPT_CACHING_1H".to_owned(), "0".into())]);
    assert_eq!(prompt_cache_1h(&launch, &chosen), None);
}

/// What `launch` sets `ENABLE_PROMPT_CACHING_1H` to over the workspace's
/// `environment`, if it touches it.
fn prompt_cache_1h(
    launch: &AgentLaunch,
    environment: &BTreeMap<String, OsString>,
) -> Option<Option<OsString>> {
    let mut command = Command::new("true");
    launch.apply(&mut command, environment);
    command
        .as_std()
        .get_envs()
        .find(|(name, _)| *name == "ENABLE_PROMPT_CACHING_1H")
        .map(|(_, value)| value.map(ToOwned::to_owned))
}
