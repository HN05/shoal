use super::*;
use crate::{
    daemon::approvals::{DecisionStatus, Lifetime},
    secrets::Secret,
    test_support::{manager, repository},
};

fn configure(manager: &Manager, command: &str, lifetime: Lifetime) {
    let mut config = crate::config::Config::default();
    for (name, requires_approval) in [("GATED", true), ("OPEN", false)] {
        config.repository.secrets.insert(
            name.into(),
            Secret {
                command: vec![command.into()],
                tools: vec!["agent-browser".into()],
                requires_approval,
                approval_lifetime: lifetime,
            },
        );
    }
    manager.publish_config(config);
}

async fn acquire(
    manager: &Manager,
    caller: Option<&Caller>,
    name: &str,
    reason: Option<&str>,
) -> Result<Allocation<SecretGrant>> {
    manager
        .acquire_secret(
            "worker",
            name.into(),
            SecretRequest {
                reason: reason.map(Into::into),
                source: None,
            },
            caller,
        )
        .await
}

fn granted(outcome: Allocation<SecretGrant>) -> Option<bool> {
    match outcome {
        Allocation::Granted(grant) => Some(grant.approved),
        _ => None,
    }
}

fn pending(outcome: Allocation<SecretGrant>) -> ApprovalRequest {
    match outcome {
        Allocation::Approval(request) => *request,
        _ => panic!("expected an approval request"),
    }
}

async fn setup() -> (tempfile::TempDir, std::sync::Arc<Manager>, Caller) {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let caller = Caller {
        workspace_id: workspace.id,
        execution: None,
    };
    (temp, manager, caller)
}

#[tokio::test]
async fn gated_secrets_need_a_decision_bound_to_their_source() -> Result<()> {
    let (_temp, manager, caller) = setup().await;
    configure(&manager, "token", Lifetime::Lease);
    let scoped = Some(&caller);
    assert!(acquire(&manager, scoped, "MISSING", None).await.is_err());
    assert_eq!(
        granted(acquire(&manager, scoped, "OPEN", None).await?),
        Some(false)
    );
    assert_eq!(
        granted(acquire(&manager, None, "GATED", None).await?),
        Some(false)
    );
    let Err(error) = acquire(&manager, scoped, "GATED", None).await else {
        panic!("a first request needs a reason");
    };
    assert!(
        format!("{error:#}").contains("shoal acquire secret GATED --reason"),
        "{error:#}"
    );

    let request = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
    assert_eq!(request.target, Target::Secret("GATED".into()));
    // The launcher's check names the same request until the user decides.
    assert_eq!(
        pending(acquire(&manager, scoped, "GATED", None).await?).id,
        request.id
    );
    manager
        .decide_approval(request.id, true, None, Default::default())
        .await?;
    assert_eq!(
        granted(acquire(&manager, scoped, "GATED", None).await?),
        Some(true)
    );

    configure(&manager, "other-token", Lifetime::Lease);
    assert!(acquire(&manager, scoped, "GATED", None).await.is_err());
    // A launcher started before the change cannot use the new grant.
    let stale = SecretRequest {
        reason: None,
        source: Some(SecretSpecification {
            command: vec!["token".into()],
            tools: vec!["agent-browser".into()],
        }),
    };
    let Err(error) = manager
        .acquire_secret("worker", "OPEN".into(), stale, scoped)
        .await
    else {
        panic!("a stale source must be refused");
    };
    assert!(format!("{error:#}").contains("changed since"), "{error:#}");
    configure(&manager, "token", Lifetime::Lease);
    manager.release_secret("worker", "GATED".into()).await?;
    assert!(acquire(&manager, scoped, "GATED", None).await.is_err());
    assert!(
        manager
            .release_secret("worker", "GATED".into())
            .await
            .is_err()
    );

    let denied = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
    manager
        .decide_approval(denied.id, false, None, Default::default())
        .await?;
    let retry = pending(acquire(&manager, scoped, "GATED", None).await?);
    assert_eq!(
        (retry.id, retry.status),
        (denied.id, DecisionStatus::Denied)
    );
    Ok(())
}

#[tokio::test]
async fn workspace_grants_outlast_release() -> Result<()> {
    let (_temp, manager, caller) = setup().await;
    configure(&manager, "token", Lifetime::Workspace);
    let scoped = Some(&caller);
    let request = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
    manager
        .decide_approval(request.id, true, None, Default::default())
        .await?;
    manager.release_secret("worker", "GATED".into()).await?;
    assert_eq!(
        granted(acquire(&manager, scoped, "GATED", None).await?),
        Some(true)
    );
    Ok(())
}
