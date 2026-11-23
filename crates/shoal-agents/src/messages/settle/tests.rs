use super::*;

fn sent_at(times: &[i64]) -> Vec<Alert> {
    times
        .iter()
        .enumerate()
        .map(|(n, &created_at)| Alert {
            id: i64::try_from(n).unwrap(),
            created_at: UnixTime::from_seconds(created_at),
            kind: shoal_daemon_base::alerts::AlertKind::Message,
            sender: shoal_daemon_base::alerts::Sender::User,
            recipient: None,
            space: None,
            space_id: None,
            number: None,
            message: format!("message {n}"),
            read: false,
            files: Vec::new(),
            machine: None,
        })
        .collect()
}

fn at(seconds: i64) -> UnixTime {
    UnixTime::from_seconds(seconds)
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

#[test]
fn a_run_settles_once_none_arrives_for_the_window() {
    let run = sent_at(&[100, 103]);
    assert_eq!(held_until(&run, secs(5), at(103)), Some(at(108)));
    assert_eq!(held_until(&run, secs(5), at(108)), None);
    // Without a window, each message is delivered at once.
    assert_eq!(held_until(&run, secs(0), at(103)), None);
    assert_eq!(held_until(&[], secs(5), at(100)), None);
}

#[test]
fn a_steady_run_settles_at_most_six_windows_after_its_first_message() {
    let run = sent_at(&[100, 104, 108, 112, 116, 120, 124, 128]);
    assert_eq!(held_until(&run, secs(5), at(128)), Some(at(130)));
    assert_eq!(held_until(&run, secs(5), at(130)), None);
}
