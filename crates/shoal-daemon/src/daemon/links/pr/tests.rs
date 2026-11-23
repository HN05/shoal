use super::*;
use serde_json::json;
use shoal_ids::space_ids::SpaceId;
use shoal_links::pr::RegistrationChange;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn unwatch_removes_only_the_selected_pr_even_when_cleanup_is_disabled() {
    use crate::daemon::test_support::{git, manager, repository};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
        .write_local_config(&repo.id, Some("[cleanup.pr]\nenabled=false\n".into()))
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager.store.run(move |db| {
            db.execute("INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                rusqlite::params![id, json!({"urls":["https://github.com/team/repo/pull/1", "https://github.com/team/repo/pull/2"]}).to_string()])?;
            Ok(())
        }).await.unwrap();
    assert!(
        manager
            .links
            .set_pr(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Action::Unwatch {
                    url: "https://github.com/team/repo/pull/3".parse().unwrap()
                }
            )
            .await
            .is_err()
    );
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Action::Unwatch {
                url: "https://github.com/team/repo/pull/1".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(
        matches!(manager.links.pr_registration(&workspace.id).await.unwrap().unwrap().kind,
            RegistrationKind::Watch { urls, .. }
                if urls.iter().map(|url| url.url()).eq(["https://github.com/team/repo/pull/2"]))
    );
    manager
        .links
        .set_pr(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Action::Unwatch {
                url: "2".parse().unwrap(),
            },
        )
        .await
        .unwrap();
    assert!(
        manager
            .links
            .pr_registration(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn a_chat_removed_before_its_pr_link_is_written_keeps_none() {
    let (_temp, manager) = crate::daemon::test_support::manager().await;
    let url = "https://forge.example/team/repo/pulls/8".to_owned();
    let change = RegistrationChange::new(
        None,
        Some(Registration {
            kind: RegistrationKind::Watch {
                urls: vec![url.parse().unwrap()],
                merged_head: None,
            },
            error: None,
        }),
        None,
    );
    // The chat's removal, which deletes its registration, ran first.
    let links = manager.links.clone();
    let refused = manager
        .store
        .write(move |tx| links.record_change(tx, &SpaceId::from("removed-chat"), change))
        .await;
    assert!(refused.is_err());
    assert!(
        manager
            .links
            .pr_registration(&SpaceId::from("removed-chat"))
            .await
            .unwrap()
            .is_none()
    );
}
