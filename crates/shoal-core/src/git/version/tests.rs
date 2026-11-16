use std::{os::unix::process::ExitStatusExt, process::ExitStatus};

use super::*;

const HINT: &str = "or newer is required";

fn exited(code: i32, stdout: &str, stderr: &str) -> io::Result<Output> {
    Ok(Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.into(),
        stderr: stderr.into(),
    })
}

#[test]
fn releases_read_major_and_minor() {
    for (version, release) in [
        ("git version 2.54.0\n", Some((2, 54))),
        ("git version 2.39.5 (Apple Git-154)", Some((2, 39))),
        ("git version 2.37.1.windows.1", Some((2, 37))),
        ("git version 3.0", Some((3, 0))),
        ("git version two", None),
        ("2.54.0", None),
    ] {
        assert_eq!(parse(version), release, "{version}");
    }
}

#[tokio::test]
async fn only_refused_arguments_or_an_older_git_name_the_release() {
    let installed = installed().await.expect("the installed Git's release");
    let newer = (installed.0, installed.1 + 1);
    let denied = "fatal: unknown error occurred while reading the configuration files";
    for (code, needed, hinted) in [
        (128, (2, 0), false),
        (128, newer, true),
        (USAGE, (2, 0), true),
    ] {
        let error = checked_output("git merge-tree", exited(code, "", denied), needed)
            .await
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains(denied), "{error}");
        assert_eq!(error.contains(HINT), hinted, "{error}");
    }
    let stdout = checked_output("git merge-tree", exited(0, "tree\n", ""), newer).await;
    assert_eq!(stdout.unwrap(), "tree\n");
}
