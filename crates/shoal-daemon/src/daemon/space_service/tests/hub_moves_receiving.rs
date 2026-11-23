use crate::daemon::hub_moves::receiving::*;
use crate::transfer::ImportedMove;
use std::os::unix::process::ExitStatusExt as _;

fn output(stdout: &str, stderr: &str) -> std::process::Output {
    std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

#[test]
fn an_import_reports_on_its_last_line_of_output() {
    let printed = "Setting up\n{\"outcome\":\"refused\",\"error\":\"already here\"}\n\n";
    let ImportedMove::Refused { error } = reported(&output(printed, "")).unwrap() else {
        panic!("expected a refusal");
    };
    assert_eq!(error, "already here");
}

#[test]
fn an_import_that_ends_without_a_report_fails_with_what_it_wrote() {
    let error = reported(&output("", "the daemon went away\n")).unwrap_err();
    assert!(
        format!("{error:#}").contains("the daemon went away"),
        "{error:#}"
    );
}
