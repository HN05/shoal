use super::*;

#[tokio::test(start_paused = true)]
async fn output_mode_suppresses_progress_even_when_work_crosses_the_draw_delay() {
    use std::ffi::OsStr;
    for (json, terminal, term) in [
        (true, true, Some(OsStr::new("xterm-256color"))),
        (false, true, Some(OsStr::new("dumb"))),
        (false, false, Some(OsStr::new("xterm-256color"))),
        (true, false, None),
    ] {
        let mut output = Vec::new();
        let spinner = Spinner::new(shows(json, terminal, term), "Working", &mut output);
        let result = spin(spinner, async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            7
        })
        .await;
        assert_eq!(result, 7);
        assert!(output.is_empty(), "{json} {terminal} {term:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn completed_work_never_draws_progress() {
    let mut output = Vec::new();
    let spinner = Spinner::new(true, "Working", &mut output);
    assert_eq!(spin(spinner, async { 7 }).await, 7);
    assert!(output.is_empty());
}

#[tokio::test(start_paused = true)]
async fn pending_work_draws_frames_and_clears_before_returning_errors() {
    let mut output = Vec::new();
    let spinner = Spinner::new(true, "Working", &mut output);
    let result = spin(spinner, async {
        tokio::time::sleep(Duration::from_millis(1100)).await;
        Err::<(), _>("controlled failure")
    })
    .await;
    assert_eq!(result, Err("controlled failure"));
    let text = String::from_utf8(output).unwrap();
    assert!(text.starts_with("\r| Working (0s)"), "{text:?}");
    assert!(
        text.contains("/ Working") && text.contains("Working (1s)"),
        "{text:?}"
    );
    assert!(text.ends_with(" \r"), "{text:?}");
}

#[tokio::test(start_paused = true)]
async fn a_cleared_frame_returns_below_with_the_new_message_and_the_same_clock() {
    let mut output = Vec::new();
    let mut spinner = Spinner::new(true, "Starting", &mut output);
    spinner.tick().await;
    spinner.clear();
    spinner.clear();
    spinner.set_message("Running setup");
    tokio::time::sleep(Duration::from_secs(1)).await;
    spinner.tick().await;
    drop(spinner);
    let text = String::from_utf8(output).unwrap();
    let (first, second) = ("| Starting (0s)", "/ Running setup (1s)");
    let erase = |frame: &str| format!("\r{}\r", " ".repeat(frame.len()));
    let expected = format!("\r{first}{}\r{second}{}", erase(first), erase(second));
    assert_eq!(text, expected);
}
