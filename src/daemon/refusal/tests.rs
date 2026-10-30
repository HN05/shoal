use anyhow::Context as _;

use super::*;

#[test]
fn a_refusal_keeps_its_message_and_its_reason_through_context() {
    let error: anyhow::Error = Refusal::new(Reason::NotFound, "unknown workspace: x").into();
    assert_eq!(format!("{error:#}"), "unknown workspace: x");
    let error = Err::<(), _>(error).context("remove x").unwrap_err();
    assert_eq!(Reason::of(&error), Some(Reason::NotFound));
    assert_eq!(Reason::of(&anyhow::anyhow!("other")), None);
}

#[test]
fn a_wrapped_error_keeps_its_whole_message() {
    let inner = Err::<(), _>(anyhow::anyhow!("inner"))
        .context("outer")
        .unwrap_err();
    let error = Refusal::wrap(Reason::NotReady, inner);
    assert_eq!(format!("{error:#}"), "outer: inner");
    assert_eq!(Reason::of(&error), Some(Reason::NotReady));
}
