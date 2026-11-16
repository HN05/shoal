use super::*;

#[test]
fn an_empty_recorded_narrowing_is_written_as_none() {
    let narrowing = PermissionsConfig::assigned(&["push=propose".to_owned()]).unwrap();
    assert_eq!(
        Narrowing::Recorded(narrowing).written(),
        (Vec::new(), Some(narrowing))
    );
    assert_eq!(
        Narrowing::Recorded(Default::default()).written(),
        (Vec::new(), None)
    );
}

#[test]
fn a_chat_beside_no_terminal_or_a_scoped_process_runs_detached() {
    use ChatPlace::{Background, Beside, Detached, Here};
    assert_eq!(Beside.for_starter(false, true), Beside);
    assert_eq!(Beside.for_starter(false, false), Detached);
    assert_eq!(Beside.for_starter(true, true), Detached);
    assert_eq!(Here.for_starter(true, false), Here);
    assert_eq!(Background.for_starter(true, false), Background);
}
