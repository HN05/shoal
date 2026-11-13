use super::*;
use crate::protocol::MAX_FRAME;

#[tokio::test]
async fn an_oversized_reply_is_answered_with_an_error_saying_so() {
    let (mut writer, reader) = tokio::io::duplex(4 * MAX_FRAME);
    let env = [("VALUE".to_owned(), "x".repeat(MAX_FRAME))].into();
    let error = reply(&mut writer, 7, Body::WorkspaceEnv(env))
        .await
        .unwrap_err();
    assert!(error.is::<protocol::FrameTooLarge>(), "{error:#}");
    drop(writer);
    let response: Response = protocol::read(&mut tokio::io::BufReader::new(reader))
        .await
        .unwrap();
    assert_eq!(response.id, 7);
    let Body::Error { code, .. } = response.body else {
        panic!("expected an error reply");
    };
    assert_eq!(code, ErrorCode::OperationFailed);
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
        ApprovalRequest, ApprovalRequestExt, HoldSpecification, Lifetime, Specification, Target,
    };

    let space = crate::client::space_ids::SpaceId::from("space");
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
