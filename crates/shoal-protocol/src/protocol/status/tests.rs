use super::*;

#[test]
fn a_stall_names_a_privacy_prompt_only_on_macos() {
    let stall = AccessStall {
        directory: "/repo".into(),
        path: "/repo/workspace".into(),
        since: crate::time::UnixTime::from_seconds(0),
        reason: "did not finish within 30s".into(),
    };
    let message = stall.message();
    assert_eq!(
        message.contains("privacy prompt"),
        cfg!(target_os = "macos"),
        "{message}"
    );
}
