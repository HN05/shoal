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
