use serde_json::json;

use crate::{
    forge::raw::{HttpMethod, RawRequest},
    protocol::ItemContext,
    test_support::{git, install_test_script, manager, repository},
};

#[tokio::test]
async fn agents_read_as_their_account_and_write_only_when_allowed() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "api".into(), None, None, None)
        .await
        .unwrap();
    git(
        &path,
        &["remote", "add", "origin", "https://github.com/team/repo"],
    );
    let gh = root.path().join("gh-agent");
    let log = root.path().join("log");
    install_test_script(
        &gh,
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nprintf 'HTTP/2.0 200 OK\\n\\n{{\"ok\":true}}'\n",
            log.display()
        ),
    );
    let auth = format!("[agent.forge_clis]\ngh = '{}'\n", gh.display());
    manager
        .write_local_config(&repo.id, Some(auth.clone()))
        .await
        .unwrap();
    let context = ItemContext::Workspace(workspace.id.clone());
    let request = |method| RawRequest {
        method,
        path: "repos/{owner}/{repo}".into(),
        body: None,
    };

    let read = manager
        .forge_api(&context, &request(HttpMethod::Get), None, true)
        .await
        .unwrap();
    assert_eq!(read, json!({"ok": true}));
    let error = manager
        .forge_api(&context, &request(HttpMethod::Patch), None, true)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("api_writes"), "{error:#}");
    let sent = std::fs::read_to_string(&log).unwrap();
    assert_eq!(sent.lines().count(), 1, "{sent}");
    assert!(sent.contains("-X GET repos/team/repo"), "{sent}");

    manager
        .write_local_config(
            &repo.id,
            Some(format!("{auth}[permissions]\napi_writes = 'direct'\n")),
        )
        .await
        .unwrap();
    manager
        .forge_api(&context, &request(HttpMethod::Patch), None, true)
        .await
        .unwrap();
    let sent = std::fs::read_to_string(&log).unwrap();
    assert!(sent.contains("-X PATCH repos/team/repo"), "{sent}");

    // Another repository is only read, whatever the workspace's allows.
    let read = manager
        .forge_api(&context, &request(HttpMethod::Get), Some("team/tool"), true)
        .await
        .unwrap();
    assert_eq!(read, json!({"ok": true}));
    let error = manager
        .forge_api(
            &context,
            &request(HttpMethod::Patch),
            Some("team/tool"),
            true,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("GET requests to another repository"),
        "{error:#}"
    );
    let sent = std::fs::read_to_string(&log).unwrap();
    assert_eq!(sent.lines().count(), 3, "{sent}");
    assert!(sent.contains("-X GET repos/team/tool"), "{sent}");
}
