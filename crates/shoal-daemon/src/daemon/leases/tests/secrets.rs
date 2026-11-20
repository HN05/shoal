//! Secrets as the approvals store grants them.
use anyhow::Result;

use crate::daemon::{
    allocation::Allocation,
    approvals::ApprovalRequest,
    manager::Manager,
    scope::Caller,
    test_support::{manager, repository},
};
use shoal_core::secrets::Secret;
use shoal_protocol::config::repo::Lifetime;
use shoal_protocol::secrets::{SecretGrant, SecretRequest};

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
        .leases
        .acquire_secret(
            manager,
            &manager.named("worker").await,
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
        space_id: workspace.id,
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    (temp, manager, caller)
}

#[tokio::test]
async fn workspace_grants_outlast_release() -> Result<()> {
    let (_temp, manager, caller) = setup().await;
    configure(&manager, "token", Lifetime::Workspace);
    let scoped = Some(&caller);
    let request = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
    manager
        .decide_approval(request.number, true, None, Default::default(), None)
        .await?;
    let worker = manager.named("worker").await;
    (manager
        .leases
        .release_secret(&*manager, &worker, "GATED".into()))
    .await?;
    assert_eq!(
        granted(acquire(&manager, scoped, "GATED", None).await?),
        Some(true)
    );
    Ok(())
}
