use std::ffi::OsString;

use super::*;

#[test]
fn a_relaunch_keeps_whether_a_narrowing_was_asked_for_or_recorded() {
    let assignments = ["push=propose".to_owned()];
    let narrowing = crate::config::permissions::PermissionsConfig::assigned(&assignments).unwrap();
    assert_eq!(
        Narrowing::Requested(narrowing).args(),
        [OsString::from("--permission"), "push=propose".into()]
    );
    let recorded = Narrowing::Recorded(narrowing).args();
    assert_eq!(recorded[0], "--recorded-narrowing");
    let json = recorded[1].to_str().unwrap();
    assert_eq!(
        serde_json::from_str::<crate::config::permissions::PermissionsConfig>(json).unwrap(),
        narrowing
    );
    assert!(Narrowing::Recorded(Default::default()).args().is_empty());
}
