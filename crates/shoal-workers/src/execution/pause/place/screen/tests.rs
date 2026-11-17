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
    let rows = screen().rows(80);
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
    let unsent = screen.rows(80);
    assert!(shows(&unsent, "pid 42"), "{unsent:?}");
    assert!(shows(&unsent, "the daemon is away"), "{unsent:?}");
    screen.wake = Some(Wake::Waking);
    let waking = screen.rows(80);
    assert!(shows(&waking, "pid 42"), "{waking:?}");
    assert!(!shows(&waking, "the daemon is away"), "{waking:?}");
}

#[test]
fn a_redraw_replaces_the_rows_the_block_took_before() {
    let mut screen = screen();
    let first = screen.frame(80);
    assert!(first.starts_with(CLEAR_BELOW), "{first:?}");
    let before = screen.rows(80).len();
    screen
        .notes
        .push("the restore also waits for pid 42".into());
    let second = screen.frame(80);
    assert!(
        second.starts_with(&format!("\r\x1b[{before}A{CLEAR_BELOW}")),
        "{second:?}"
    );
    let after = second.matches("\r\n").count();
    assert_eq!(after, before + 1, "{second:?}");
    let third = screen.frame(80);
    assert!(third.starts_with(&format!("\r\x1b[{after}A")), "{third:?}");
}

#[test]
fn the_block_wraps_long_lines_to_the_terminal_and_redraws_over_every_row() {
    let note = "the restore also waits until the agent's remaining processes exit: \
                pid 4242 (cargo test) still runs; shoal stop --processes stops it";
    let mut screen = screen();
    screen.notes.push(note.into());
    let rows = screen.rows(40);
    assert!(rows.iter().all(|row| row.chars().count() <= 40), "{rows:?}");
    let words = |text: &str| {
        text.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let shown = rows.iter().flat_map(|row| words(row)).collect::<Vec<_>>();
    assert!(
        shown
            .windows(words(note).len())
            .any(|run| run == words(note)),
        "{rows:?}"
    );
    screen.frame(40);
    let redraw = screen.frame(40);
    assert!(
        redraw.starts_with(&format!("\r\x1b[{}A", rows.len())),
        "{redraw:?}"
    );
}

#[test]
fn a_word_wider_than_the_terminal_breaks_within_it() {
    let rows = wrap(&format!(" {}", "x".repeat(50)), 20);
    assert!(rows.iter().all(|row| row.chars().count() <= 20), "{rows:?}");
    assert_eq!(rows.concat().matches('x').count(), 50, "{rows:?}");
}
