use super::*;
use crate::stand_in::Fixture;
use shoal_protocol::approvals::DecisionStatus;

/// A repository with a gated and an open secret, each read by `command`.
fn secrets(command: &str) -> String {
    format!(
        "[secrets.GATED]\nvalue = {{ cmd = ['{command}'] }}\ntools = ['agent-browser']\nrequires_approval = true\n\
         [secrets.OPEN]\nvalue = {{ cmd = ['{command}'] }}\ntools = ['agent-browser']\n"
    )
}

async fn acquire(
    fixture: &Fixture,
    caller: Option<&Caller>,
    name: &str,
    request: SecretRequest,
) -> Result<Allocation<SecretGrant>> {
    let (leases, selector) = (&fixture.leases, fixture.selector());
    (leases.acquire_secret(&fixture.daemon, &selector, name.into(), request, caller)).await
}

/// A request for `name` as `caller`, giving `reason`.
async fn request(
    fixture: &Fixture,
    caller: Option<&Caller>,
    name: &str,
    reason: Option<&str>,
) -> Result<Allocation<SecretGrant>> {
    let request = SecretRequest {
        reason: reason.map(Into::into),
        source: None,
    };
    acquire(fixture, caller, name, request).await
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

#[tokio::test]
async fn gated_secrets_need_a_decision_bound_to_their_source() -> Result<()> {
    let fixture = Fixture::new("", &secrets("token")).await;
    let caller = Caller::of_space(fixture.workspace.id.clone());
    let scoped = Some(&caller);
    assert!(request(&fixture, scoped, "MISSING", None).await.is_err());
    assert_eq!(
        granted(request(&fixture, scoped, "OPEN", None).await?),
        Some(false)
    );
    assert_eq!(
        granted(request(&fixture, None, "GATED", None).await?),
        Some(false)
    );
    let Err(unexplained) = request(&fixture, scoped, "GATED", None).await else {
        panic!("a request without a reason was taken");
    };
    assert!(
        format!("{unexplained:#}").contains("shoal acquire secret GATED --reason"),
        "{unexplained:#}"
    );

    let asked = pending(request(&fixture, scoped, "GATED", Some("sign in")).await?);
    assert_eq!(asked.target, Target::Secret("GATED".into()));
    let token = SecretSpecification {
        value: shoal_protocol::config::value::Value::Cmd {
            cmd: vec!["token".into()],
        },
        tools: vec!["agent-browser".into()],
    };
    assert_eq!(asked.specification, Specification::Secret(token.clone()));
    assert_eq!(fixture.daemon.announced.lock().unwrap().len(), 1);
    // The launcher's check names the same request until the user decides.
    assert_eq!(
        pending(request(&fixture, scoped, "GATED", None).await?).key,
        asked.key
    );
    let target = Target::Secret("GATED".into());
    fixture
        .records
        .decide(&target, "GATED", DecisionStatus::Approved);
    assert_eq!(
        granted(request(&fixture, scoped, "GATED", None).await?),
        Some(true)
    );

    // A changed source needs a new decision, and a launcher started before
    // the change cannot use it.
    fixture.configure(&secrets("other-token"));
    assert!(
        request(&fixture, scoped, "GATED", None).await.is_err(),
        "the grant covers the source it was approved for"
    );
    let stale = SecretRequest {
        reason: None,
        source: Some(token),
    };
    assert!(acquire(&fixture, scoped, "OPEN", stale).await.is_err());

    let (leases, selector) = (&fixture.leases, fixture.selector());
    let release = || leases.release_secret(&fixture.daemon, &selector, "GATED".into());
    release().await?;
    assert!(release().await.is_err(), "nothing is left to release");
    Ok(())
}
