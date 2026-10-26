use super::*;

#[test]
fn stopped_lines_name_the_cause_and_only_protection_saves_it() {
    let protection = Halt::Protection {
        recover: false,
        reason: "free disk space is below 2 GiB".into(),
        resumes_when: "free disk space reaches 5 GiB".into(),
    };
    assert_eq!(
        protection.saved_reason(),
        Some("free disk space is below 2 GiB")
    );
    assert_eq!(
        protection.manual_restore(&protection.restore("worker", "id")),
        "shoal: agent stopped: free disk space is below 2 GiB; once free disk space reaches 5 GiB, restore with shoal resume worker --execution id"
    );
    let stop = Halt::Pause {
        reason: Some("stopped by shoal stop".into()),
        removed: false,
    };
    assert_eq!(stop.saved_reason(), None);
    assert_eq!(stop.stated_reason(), ": stopped by shoal stop");
    assert_eq!(
        stop.manual_restore(&stop.restore("worker", "id")),
        "shoal: agent stopped: stopped by shoal stop; restore with shoal resume worker --execution id"
    );
    let removal = Halt::Pause {
        reason: Some("the workspace is being removed".into()),
        removed: true,
    };
    assert_eq!(
        removal.manual_restore(&removal.restore("worker", "id")),
        "shoal: agent stopped: the workspace is being removed; restore with shoal work restore worker, then shoal resume worker --execution id"
    );
    let pause = Halt::Pause {
        reason: None,
        removed: false,
    };
    assert_eq!(pause.stated_reason(), "");
}
