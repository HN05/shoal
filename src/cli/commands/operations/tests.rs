use super::{Failed, error, failure};
use crate::{
    daemon::approvals::Undecided,
    execution::SetupVerificationFailure,
    protocol::{ErrorCode, RemoteError, operations::Failure},
};

#[test]
fn failures_come_back_as_the_errors_their_commands_handle() {
    let unverified = error(failure(SetupVerificationFailure { exit_code: 7 }.into()));
    assert_eq!(
        unverified
            .downcast_ref::<SetupVerificationFailure>()
            .map(|failure| failure.exit_code),
        Some(7)
    );
    let refused = RemoteError {
        code: ErrorCode::NotReady,
        message: "api is not ready".into(),
    };
    let refused = error(failure(refused.into()));
    let remote = refused
        .downcast_ref::<RemoteError>()
        .map(|error| &error.code);
    assert_eq!(remote, Some(&ErrorCode::NotReady));
    assert_eq!(format!("{refused:#}"), "api is not ready");
    let hook = Failure::HookFailed {
        message: "post_setup exited with 1; the workspace is kept".into(),
    };
    let hook = error(failure(Failed(hook).into()));
    assert!(matches!(
        hook.downcast_ref::<Failed>(),
        Some(Failed(Failure::HookFailed { .. }))
    ));
    assert_eq!(
        format!("{hook:#}"),
        "post_setup exited with 1; the workspace is kept"
    );
    let other = error(failure(anyhow::anyhow!("inner").context("outer")));
    assert_eq!(format!("{other:#}"), "outer: inner");
    assert!(other.downcast_ref::<Undecided>().is_none());
}
