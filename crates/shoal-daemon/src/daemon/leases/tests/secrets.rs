//! Secrets as the approvals store grants them.
use crate::daemon::scope::Access;
use anyhow::Result;

use crate::daemon::{
    allocation::Allocation,
    approvals::ApprovalRequest,
    manager::Manager,
    scope::Caller,
    test_support::{manager, repository},
};
use shoal_core::secrets::Secret;
use shoal_protocol::config::{
    repo::Lifetime,
    value::{Value, ValueKey},
};
use shoal_protocol::secrets::{SecretGrant, SecretRequest};
use shoal_protocol::space_names::SpaceSelector;

fn configure(manager: &Manager, command: &str, lifetime: Lifetime) {
    let value = Value::Cmd {
        cmd: vec![command.into()],
        on: None,
    };
    configure_value(manager, value, lifetime);
}

/// A gated and an open secret, each given `value`.
fn configure_value(manager: &Manager, value: Value, lifetime: Lifetime) {
    let mut config = crate::config::Config::default();
    for (name, requires_approval) in [("GATED", true), ("OPEN", false)] {
        config.repository.secrets.insert(
            name.into(),
            Secret {
                value: value.clone(),
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
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
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
        access: Access::Full,
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
        .coordination
        .decide_approval(
            &*manager,
            request.number,
            true,
            None,
            Default::default(),
            None,
        )
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

/// What `caller` reads for the secret `name`, as a launch resolved `value`.
async fn written(
    manager: &Manager,
    caller: Option<&Caller>,
    name: &str,
    value: &Value,
) -> Result<String> {
    let key = ValueKey::Secret {
        name: name.into(),
        source: shoal_protocol::approvals::SecretSpecification {
            value: value.recorded(),
            tools: vec!["agent-browser".into()],
        },
    };
    let worker = manager.named("worker").await;
    Ok(manager.config_value(&worker, key, caller).await?.0)
}

#[tokio::test]
async fn a_written_secret_reaches_a_process_that_may_fetch_it() -> Result<()> {
    let (_temp, manager, caller) = setup().await;
    let value = Value::Written("s3cret".into());
    configure_value(&manager, value.clone(), Lifetime::Workspace);
    let scoped = Some(&caller);

    assert_eq!(written(&manager, scoped, "OPEN", &value).await?, "s3cret");
    assert!(written(&manager, scoped, "GATED", &value).await.is_err());
    // A launch that read another value is told to restart.
    let other = Value::Written("earlier".into());
    assert!(written(&manager, scoped, "OPEN", &other).await.is_err());

    let request = pending(acquire(&manager, scoped, "GATED", Some("sign in")).await?);
    (manager.coordination)
        .decide_approval(
            &*manager,
            request.number,
            true,
            None,
            Default::default(),
            None,
        )
        .await?;
    assert_eq!(written(&manager, scoped, "GATED", &value).await?, "s3cret");
    Ok(())
}

#[tokio::test]
async fn the_daemon_reads_no_secret_its_process_reads_itself() -> Result<()> {
    let (_temp, manager, _) = setup().await;
    let value = Value::Cmd {
        cmd: vec!["token".into()],
        on: None,
    };
    configure_value(&manager, value.clone(), Lifetime::Workspace);
    assert!(written(&manager, None, "OPEN", &value).await.is_err());
    Ok(())
}

#[tokio::test]
async fn a_chat_reads_a_written_secret_as_a_workspace_does() -> Result<()> {
    let (_temp, manager, _) = setup().await;
    let value = Value::Written("s3cret".into());
    configure_value(&manager, value.clone(), Lifetime::Workspace);
    let chat = crate::daemon::test_support::chat(&manager, "planning", "chat", None, None).await;
    let key = ValueKey::Secret {
        name: "OPEN".into(),
        source: shoal_protocol::approvals::SecretSpecification {
            value: value.recorded(),
            tools: vec!["agent-browser".into()],
        },
    };
    let caller = Caller {
        space_id: chat.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let chat = SpaceSelector::id(&chat.id);

    let read = manager.config_value(&chat, key, Some(&caller)).await?;

    assert_eq!(read.0, "s3cret");
    Ok(())
}
