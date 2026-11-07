use super::*;

fn leftover(pid: u32, command: &str, started_at: UnixTime) -> Leftover {
    Leftover {
        pid,
        command: command.into(),
        directory: None,
        started_at,
    }
}

#[test]
fn a_notice_names_each_leftover_and_how_to_stop_them() {
    let long = format!(
        "target/debug/shoal run --workspace summary -- sh -c {}",
        "x".repeat(80)
    );
    let now = UnixTime::from_seconds(1_000_000);
    let several = notice(
        "workspace",
        &[
            leftover(4121, &long, now - Duration::from_secs(41 * 60)),
            leftover(4125, "sh", now - Duration::from_secs(30)),
        ],
        now,
    );
    for part in [
        "2 processes from this workspace",
        "sh -c xxxxxxxxxxxxxxxxxxxxxxxxxxxx … (PID 4121, 41 min), sh (PID 4125, under 1 min).",
    ] {
        assert!(several.contains(part), "{part:?} in {several}");
    }
    let one = notice(
        "chat",
        &[leftover(
            7,
            "sleep 300",
            now - Duration::from_secs(3 * 86_400),
        )],
        now,
    );
    for part in ["1 process from this chat", "sleep 300 (PID 7, 3 days)."] {
        assert!(one.contains(part), "{part:?} in {one}");
    }
}

#[test]
fn running_times_name_the_largest_whole_unit() {
    assert_eq!(running_time(Duration::from_secs(59)), "under 1 min");
    assert_eq!(running_time(Duration::from_secs(60)), "1 min");
    assert_eq!(running_time(Duration::from_secs(119 * 60)), "119 min");
    assert_eq!(running_time(Duration::from_secs(5 * 3600)), "5 h");
    assert_eq!(running_time(Duration::from_secs(50 * 3600)), "2 days");
}
