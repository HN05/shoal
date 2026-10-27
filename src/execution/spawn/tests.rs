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
        proxy: None,
    }
}

#[tokio::test]
#[cfg(feature = "proxy")]
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
    // Removing it stops it.
    assert!(!launch.restart_proxy(None, &environment).await.unwrap());
    assert!(launch.proxy.is_none());
}
