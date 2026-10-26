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
        let result = run_on(
            json,
            terminal,
            term,
            "Working",
            async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                7
            },
            &mut output,
        )
        .await;
        assert_eq!(result, 7);
        assert!(output.is_empty(), "{json} {terminal} {term:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn completed_work_never_draws_progress() {
    let mut output = Vec::new();
    assert_eq!(render("Working", async { 7 }, &mut output).await, 7);
    assert!(output.is_empty());
}

#[tokio::test(start_paused = true)]
async fn pending_work_draws_frames_and_clears_before_returning_errors() {
    let mut output = Vec::new();
    let result = render(
        "Working",
        async {
            tokio::time::sleep(Duration::from_millis(1100)).await;
            Err::<(), _>("controlled failure")
        },
        &mut output,
    )
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
