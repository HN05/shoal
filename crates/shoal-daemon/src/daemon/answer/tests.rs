use super::*;
use crate::protocol::MAX_FRAME;
use shoal_protocol::config::repo::Lifetime;

#[tokio::test]
async fn an_oversized_reply_arrives_in_pieces_that_join() {
    let (mut writer, reader) = tokio::io::duplex(4 * MAX_FRAME);
    let env: std::collections::BTreeMap<String, String> =
        [("VALUE".to_owned(), "x".repeat(MAX_FRAME))].into();
    reply(&mut writer, 7, Body::SpaceEnvironment(env.clone()))
        .await
        .unwrap();
    drop(writer);
    let response = protocol::read_response(&mut tokio::io::BufReader::new(reader))
        .await
        .unwrap();
    assert_eq!(response.id, 7);
    let Body::SpaceEnvironment(joined) = response.body else {
        panic!("expected the environment");
    };
    assert_eq!(joined, env);
}

#[tokio::test]
async fn an_oversized_answer_to_a_wait_arrives_in_pieces_too() {
    let (_state, manager) = crate::daemon::test_support::manager().await;
    let (client, stream) = tokio::net::UnixStream::pair().unwrap();
    let env: std::collections::BTreeMap<String, String> =
        [("VALUE".to_owned(), "x".repeat(MAX_FRAME))].into();
    let waited = async { Ok(Body::SpaceEnvironment(env.clone())) };
    // Read as it writes: the reply outgrows a socket's buffer on some hosts.
    let mut client = tokio::io::BufReader::new(client);
    let (answered, response) = tokio::join!(
        answer_wait(stream, 9, &manager, waited),
        protocol::read_response(&mut client),
    );
    answered.unwrap();
    let response = response.unwrap();
    let Body::SpaceEnvironment(joined) = response.body else {
        panic!("expected the environment");
    };
    assert_eq!(joined, env);
}

#[test]
fn refusals_keep_their_codes_under_context() {
    use anyhow::Context as _;

    use crate::refusal::Refusal;

    let error = Err::<(), _>(Refusal::new(Reason::NotFound, "refused"))
        .context("request")
        .unwrap_err();
    assert_eq!(error_code(&error), ErrorCode::NotFound, "{error:#}");
    assert_eq!(
        error_code(&anyhow::anyhow!("failed")),
        ErrorCode::OperationFailed
    );
}

#[test]
fn an_action_held_for_approval_reaches_the_client_as_its_request() {
    use crate::daemon::approvals::{
        ApprovalRequest, ApprovalRequestExt, HoldSpecification, Specification, Target,
    };

    let space = shoal_ids::space_ids::SpaceId::from("space");
    let request = ApprovalRequest::new(
        &space,
        Target::Hold("review".into()),
        "review",
        Specification::Hold(HoldSpecification {
            hold: "review".into(),
        }),
        Lifetime::Lease,
        Some("hold it"),
    );
    let error = anyhow::Error::new(Undecided(Box::new(request))).context("request");
    for body in [failure(&error), refusal(&error)] {
        let error = body.into_result().unwrap_err();
        let Undecided(request) = error.downcast_ref::<Undecided>().unwrap();
        assert_eq!(request.space_id, space);
    }
}
