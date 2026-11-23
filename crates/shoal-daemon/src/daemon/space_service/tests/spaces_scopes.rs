use crate::daemon::manager::Manager;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{scope, test_support},
    protocol::{AlertMethod, ExecutionMethod, Operation, SpaceMethod},
};

#[tokio::test]
async fn issued_scopes_are_persistent_independent_and_untracked() {
    let (root, manager) = test_support::manager().await;
    let repo = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "external".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let token = &manager.spaces.issue_scope(&selector).await.unwrap();
    let second = manager.spaces.issue_scope(&selector).await.unwrap();
    assert_ne!(token, &second);
    let caller = manager
        .coordination
        .caller(&*manager, token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(caller.space_id, workspace.id);
    assert!(caller.execution.is_none());
    // It issues and reads for the spaces its scope reaches, and acts as no
    // execution.
    for mut method in [
        Operation::from(SpaceMethod::IssueScope {
            space: selector.clone(),
        }),
        SpaceMethod::SpaceEnvironment {
            space: selector.clone(),
        }
        .into(),
    ] {
        scope::authorize(&manager, Some(token), &mut method)
            .await
            .unwrap();
    }
    for mut method in [
        Operation::from(SpaceMethod::IssueScope {
            space: SpaceSelector::id(&other.id),
        }),
        ExecutionMethod::CheckMerge.into(),
        AlertMethod::ListAlerts {
            to: crate::protocol::AlertsFor::User,
            unread_only: true,
            limit: 50,
        }
        .into(),
    ] {
        assert!(
            scope::authorize(&manager, Some(token), &mut method)
                .await
                .is_err()
        );
    }
    manager.store.shutdown().await;
    let restored = Manager::open(manager.paths.clone()).await.unwrap();
    assert!(
        (restored
            .coordination
            .caller(&*restored, token)
            .await
            .unwrap())
        .is_some()
    );
    restored
        .spaces
        .revoke_scope(&selector, token.clone())
        .await
        .unwrap();
    assert!(
        restored
            .coordination
            .caller(&*restored, token)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        restored
            .coordination
            .caller(&*restored, &second)
            .await
            .unwrap()
            .is_some()
    );
    let id = workspace.id.clone();
    restored
        .store
        .run(move |db| {
            db.execute("DELETE FROM spaces WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        restored
            .coordination
            .caller(&*restored, &second)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_chat_issues_tokens_apart_from_its_agents_which_none_revokes() {
    let (root, manager) = test_support::manager().await;
    let repo = test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;
    let agent = manager.spaces.chat_scope(&chat.id).await.unwrap();
    let selector = SpaceSelector::id(&chat.id);
    let issued = manager.spaces.issue_scope(&selector).await.unwrap();
    assert_eq!(
        manager
            .coordination
            .caller(&*manager, &issued)
            .await
            .unwrap()
            .unwrap()
            .space_id,
        chat.id
    );
    assert_eq!(manager.spaces.chat_scope(&chat.id).await.unwrap(), agent);
    manager
        .spaces
        .revoke_scope(&selector, issued.clone())
        .await
        .unwrap();
    assert!(
        manager
            .coordination
            .caller(&*manager, &issued)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        manager
            .spaces
            .revoke_scope(&selector, agent.clone())
            .await
            .is_err()
    );
    assert!(
        manager
            .coordination
            .caller(&*manager, &agent)
            .await
            .unwrap()
            .is_some()
    );
}
