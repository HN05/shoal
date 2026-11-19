use anyhow::Context as _;

use super::*;

#[test]
fn a_refusal_keeps_its_message_and_its_reason_through_context() {
    let error: anyhow::Error = Refusal::new(Reason::NotFound, "unknown workspace: x").into();
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

#[test]
fn a_refusal_of_an_absent_space_names_it_through_context() {
    let selector = SpaceSelector::Number(7.into());
    let error: anyhow::Error = Refusal::absent(&selector, "no space 7").into();
    let error = Err::<(), _>(error).context("stop 7").unwrap_err();
    assert_eq!(Refusal::absent_space(&error), Some(&selector));
    assert_eq!(Reason::of(&error), Some(Reason::NotFound));
    let other: anyhow::Error = Refusal::new(Reason::NotFound, "no repository").into();
    assert_eq!(Refusal::absent_space(&other), None);
}

#[test]
fn a_refusal_because_of_an_error_shows_it_and_finds_it_through_context() {
    let cause = std::io::Error::other("no such repository");
    let error: anyhow::Error = Refusal::because(Reason::NotFound, cause).into();
    let error = Err::<(), _>(error).context("add x").unwrap_err();
    assert_eq!(format!("{error:#}"), "add x: no such repository");
    assert_eq!(Reason::of(&error), Some(Reason::NotFound));
    assert!(Refusal::cause::<std::io::Error>(&error).is_some());
    assert!(Refusal::cause::<std::fmt::Error>(&error).is_none());
}
