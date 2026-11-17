use super::*;

fn screen() -> Screen {
    Screen::new(&Protection {
        reason: "idle until an update arrives".into(),
        resumes_when: "an update arrives for it".into(),
    })
}

fn shows(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

#[test]
fn the_block_shows_what_wakes_the_agent_and_why_it_sleeps() {
    let rows = screen().rows();
    assert!(
        rows.iter()
            .any(|row| row.starts_with(" Agent asleep")
                && row.ends_with(" an update arrives for it")),
        "{rows:?}"
    );
    assert!(shows(&rows, "idle until an update arrives"), "{rows:?}");
}

#[test]
fn the_block_shows_its_notes_and_only_the_latest_wake() {
    let mut screen = screen();
    screen
        .notes
        .push("the restore also waits for pid 42".into());
    screen.wake = Some(Wake::Unsent("the daemon is away".into()));
    let unsent = screen.rows();
    assert!(shows(&unsent, "pid 42"), "{unsent:?}");
    assert!(shows(&unsent, "the daemon is away"), "{unsent:?}");
    screen.wake = Some(Wake::Waking);
    let waking = screen.rows();
    assert!(shows(&waking, "pid 42"), "{waking:?}");
    assert!(!shows(&waking, "the daemon is away"), "{waking:?}");
}

#[test]
fn a_redraw_replaces_the_rows_the_block_took_before() {
    let mut screen = screen();
    let first = screen.frame();
    assert!(first.starts_with(CLEAR_BELOW), "{first:?}");
    let before = screen.rows().len();
    screen
        .notes
        .push("the restore also waits for pid 42".into());
    let second = screen.frame();
    assert!(
        second.starts_with(&format!("\r\x1b[{before}A{CLEAR_BELOW}")),
        "{second:?}"
    );
    let after = second.matches("\r\n").count();
    assert_eq!(after, before + 1, "{second:?}");
    let third = screen.frame();
    assert!(third.starts_with(&format!("\r\x1b[{after}A")), "{third:?}");
}
