use super::*;
use crate::backends::{Backend, ForgeKind};

#[test]
fn escapes_controls_and_redrawn_text_are_removed() {
    let log = concat!(
        "\x1b[1m\x1b[92m   Compiling\x1b[0m shoal v0.1.0\r\n",
        "\x1b]8;;https://example.com\x07link\x1b]8;;\x1b\\ text\x1b(B\x1b=\n",
        "Receiving objects:  50%\rReceiving objects: 100%\r\n",
        "bell\x07 and\u{9b}31m C1\n",
        "\tindented\x1b]unterminated\nnext\x1b",
    );
    assert_eq!(
        readable(log),
        concat!(
            "   Compiling shoal v0.1.0\n",
            "link text\n",
            "Receiving objects: 100%\n",
            "bell and C1\n",
            "\tindented\nnext",
        )
    );
}

#[test]
fn github_and_forgejo_logs_lose_their_timestamps() {
    let log = concat!(
        "2024-05-01T12:00:00.1234567Z ##[group]Run cargo test\n",
        "2024-05-01T12:00:01.0000000Z \x1b[36;1mcargo test\x1b[0m\n",
        "2024-05-01T12:00:02.0000000Z \n",
        "2024-05-01T12:00:03Z test result: FAILED\r\n",
        "2024-05-01T12:00:04.5Z\n",
        "2024-05-01 12:00:05 not a timestamp\n",
        "2024-05-01T12:00:06.Z neither\n",
        "⭐ Run Main Test",
    );
    let plain = concat!(
        "##[group]Run cargo test\n",
        "cargo test\n",
        "\n",
        "test result: FAILED\n",
        "\n",
        "2024-05-01 12:00:05 not a timestamp\n",
        "2024-05-01T12:00:06.Z neither\n",
        "⭐ Run Main Test",
    );
    assert_eq!(ForgeKind::GitHub.plain_log(log), plain);
    assert_eq!(ForgeKind::Forgejo.plain_log(log), plain);
}

#[test]
fn gitlab_traces_lose_their_section_markers() {
    let trace = concat!(
        "\x1b[0KRunning with gitlab-runner 17.0.0\x1b[0;m\n",
        "\x1b[0Ksection_start:1700000000:prepare_script[collapsed=true]\r",
        "\x1b[0K\x1b[0K\x1b[36;1mPreparing environment\x1b[0;m\x1b[0;m\n",
        "Running on runner-1\n",
        "\x1b[0Ksection_end:1700000001:prepare_script\r\x1b[0K\n",
        "\x1b[0Ksection_end:1700000002:step_script\r\x1b[0K",
        "\x1b[0Ksection_start:1700000002:after_script\r\x1b[0K",
        "\x1b[32;1m$ cargo test\x1b[0;m\n",
        "output\x1b[0Ksection_end:1700000003:after_script\r\x1b[0K\n",
        "\n",
        "\x1b[0Ksection_end:x:not_a_marker\r\x1b[0K\n",
        "\x1b[31;1mERROR: Job failed: exit code 101\n\x1b[0;m\n",
    );
    assert_eq!(
        ForgeKind::GitLab.plain_log(trace),
        concat!(
            "Running with gitlab-runner 17.0.0\n",
            "Preparing environment\n",
            "Running on runner-1\n",
            "$ cargo test\n",
            "output\n",
            "\n",
            "section_end:x:not_a_marker\n",
            "ERROR: Job failed: exit code 101\n",
            "\n",
        )
    );
}
