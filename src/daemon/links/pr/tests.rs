use super::*;
use serde_json::json;

#[tokio::test]
async fn unwatch_removes_only_the_selected_pr_even_when_cleanup_is_disabled() {
    use crate::test_support::{git, manager, repository};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "watch".into(), None, None, None)
        .await
        .unwrap();
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/team/repo.git",
        ],
    );
    manager
        .set_repository_config(&repo.id, Some("[cleanup.pr]\nenabled=false\n".into()))
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager.store.run(move |db| {
            db.execute("INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
                rusqlite::params![id, json!({"urls":["https://github.com/team/repo/pull/1", "https://github.com/team/repo/pull/2"]}).to_string()])?;
            Ok(())
        }).await.unwrap();
    assert!(
        manager
            .set_pr(&workspace.id, Action::Unwatch { url: "3".into() })
            .await
            .is_err()
    );
    manager
        .set_pr(&workspace.id, Action::Unwatch { url: "1".into() })
        .await
        .unwrap();
    assert!(
        matches!(manager.pr_registration(&workspace.id).await.unwrap().unwrap().kind,
            RegistrationKind::Watch { urls, .. } if urls == ["https://github.com/team/repo/pull/2"])
    );
    manager
        .set_pr(&workspace.id, Action::Unwatch { url: "2".into() })
        .await
        .unwrap();
    assert!(
        manager
            .pr_registration(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(manager.completion(&workspace.id).await.unwrap().is_none());
    assert!(workspace.path.exists());
}

#[test]
fn registration_round_trips_legacy_records() {
    for (kind, url, head) in [
        (
            RegistrationKind::Watch {
                urls: vec!["https://forge.example/team/repo/pulls/7".into()],
                merged_head: None,
            },
            Some("https://forge.example/team/repo/pulls/7"),
            None,
        ),
        (
            RegistrationKind::Acknowledgement {
                head: "abc123".into(),
            },
            None,
            Some("abc123"),
        ),
    ] {
        for error in [None, Some("lookup failed")] {
            let record = json!({"url": url, "head": head, "error": error});
            let registration: Registration = serde_json::from_value(record.clone()).unwrap();
            assert_eq!(registration.kind, kind);
            assert_eq!(registration.error.as_deref(), error);
            assert_eq!(serde_json::to_value(&registration).unwrap(), record);
        }
    }
    // Optional fields were also allowed to be absent.
    let registration: Registration = serde_json::from_value(json!({"head": "abc123"})).unwrap();
    assert_eq!(
        serde_json::to_value(registration).unwrap(),
        json!({"url": null, "head": "abc123", "error": null})
    );
}

#[test]
fn multiple_watches_round_trip_and_reject_ambiguous_or_empty_sets() {
    let value = json!({"url": null, "urls": ["first", "second"], "head": null,
            "merged_head": "commit", "error": null});
    let registration: Registration = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(registration).unwrap(), value);
    for value in [
        json!({"urls": []}),
        json!({"urls": ["same", "same"]}),
        json!({"urls": [""]}),
        json!({"url": "first", "urls": ["second"]}),
        json!({"urls": ["first"], "head": "commit"}),
        json!({"head": "commit", "merged_head": "commit"}),
    ] {
        assert!(serde_json::from_value::<Registration>(value).is_err());
    }
}

#[test]
fn registration_rejects_ambiguous_records() {
    for record in [
        json!({}),
        json!({"url": null, "head": null}),
        json!({"url": "url", "head": "head"}),
    ] {
        let error = serde_json::from_value::<Registration>(record).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expected exactly one of url or head"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn a_failed_branch_check_does_not_claim_another_branch() {
    use crate::test_support::{manager, repository};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "branch".into(), None, None, None)
        .await
        .unwrap();
    current_head(&workspace).await.unwrap();
    std::fs::write(workspace.path.join(".git"), "gitdir: missing\n").unwrap();
    let error = format!("{:#}", current_head(&workspace).await.unwrap_err());
    assert!(
        error.starts_with("cannot check the workspace's branch: "),
        "{error}"
    );
}
