use super::*;

#[test]
fn settling_messages_delay_a_wake_but_keep_an_issued_one_due() {
    let settling = Some(Duration::from_secs(5));
    assert!(!wake_messages_due(true, settling, true));
    assert!(wake_messages_due(true, None, true));
    // A message arriving after the prompt does not make the agent look
    // finished with the ones it was prompted for.
    assert!(wake_messages_due(true, settling, false));
    assert!(!wake_messages_due(false, None, true));
    assert!(!wake_messages_due(false, None, false));
}
