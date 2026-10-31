use serde_json::json;

use crate::{
    forge::{
        raw::{HttpMethod, RawRequest},
        test_support::Server,
    },
    protocol::ItemContext,
    test_support::{configure, git, manager, repository},
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
    let server = Server::start(|_| (200, Vec::new(), r#"{"ok":true}"#.into()));
    let host = &server.host;
    // Pins are this process's, which parallel tests replace; an answer stays.
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::GitHub);
    let origin = format!("http://{host}/team/repo");
    git(&path, &["remote", "add", "origin", &origin]);
    configure(
        &manager,
        &format!("[machine.forges.\"{host}\"]\ntype = \"github\"\nagent_token = \"agent\"\n"),
    )
    .await;
    let context = ItemContext::Workspace(workspace.id.clone());
    let request = |method| RawRequest {
        method,
        path: "repos/{owner}/{repo}".into(),
        body: None,
    };
    let sent = || -> Vec<String> {
        let received = server.received();
        assert!(
            received
                .iter()
                .all(|request| request.header("Authorization") == Some("Bearer agent")),
            "{received:?}"
        );
        received.into_iter().map(|request| request.line).collect()
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
    assert_eq!(sent(), ["GET /api/v3/repos/team/repo"]);

    manager
        .write_local_config(
            &repo.id,
            Some("[permissions]\napi_writes = 'direct'\n".into()),
        )
        .await
        .unwrap();
    manager
        .forge_api(&context, &request(HttpMethod::Patch), None, true)
        .await
        .unwrap();
    assert_eq!(sent()[1], "PATCH /api/v3/repos/team/repo");

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
    let sent = sent();
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert_eq!(sent[2], "GET /api/v3/repos/team/tool");
}

#[tokio::test]
async fn agents_download_from_their_forge_as_their_account() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "download".into(), None, None, None)
        .await
        .unwrap();
    let server = Server::start(|_| {
        let png = vec!["Content-Type: image/png".to_owned()];
        (200, png, b"PNG".to_vec())
    });
    let host = &server.host;
    // Pins are this process's, which parallel tests replace; an answer stays.
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::GitHub);
    let origin = format!("http://{host}/team/repo");
    git(&path, &["remote", "add", "origin", &origin]);
    configure(
        &manager,
        &format!("[machine.forges.\"{host}\"]\ntype = \"github\"\nagent_token = \"agent\"\n"),
    )
    .await;
    let url = &format!("http://{host}/user-attachments/assets/abc");

    let download = manager
        .forge_download(
            &ItemContext::Workspace(workspace.id.clone()),
            url,
            None,
            true,
        )
        .await
        .unwrap();
    assert_eq!(download.content_type.as_deref(), Some("image/png"));
    assert_eq!(download.bytes, b"PNG");
    let error = manager
        .forge_download(
            &ItemContext::Workspace(workspace.id.clone()),
            "https://example.com/shot.png",
            None,
            true,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("not on the repository's forge"),
        "{error:#}"
    );
    let received = server.received();
    assert_eq!(received.len(), 1, "{received:?}");
    assert_eq!(received[0].line, "GET /user-attachments/assets/abc");
    assert_eq!(received[0].header("Authorization"), Some("Bearer agent"));
}
