use super::*;

fn leftover(pid: u32, command: &str, started_at: u64) -> Leftover {
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
    let now = 1_000_000;
    assert_eq!(
        notice(
            "workspace",
            &[
                leftover(4121, &long, now - 41 * 60),
                leftover(4125, "sh", now - 30)
            ],
            now
        ),
        "2 processes from this workspace are still running after their parent exited: \
         target/debug/shoal run --workspace summary -- sh -c xxxxxxxxxxxxxxxxxxxxxxxxxxxx … \
         (PID 4121, 41 min), sh (PID 4125, under 1 min). Stop them with shoal stop --leftovers."
    );
    assert_eq!(
        notice("chat", &[leftover(7, "sleep 300", now - 3 * 86_400)], now),
        "1 process from this chat is still running after its parent exited: \
         sleep 300 (PID 7, 3 days). Stop it with shoal stop --leftovers."
    );
}

#[test]
fn running_times_name_the_largest_whole_unit() {
    assert_eq!(running_time(59), "under 1 min");
    assert_eq!(running_time(60), "1 min");
    assert_eq!(running_time(119 * 60), "119 min");
    assert_eq!(running_time(5 * 3600), "5 h");
    assert_eq!(running_time(50 * 3600), "2 days");
}
