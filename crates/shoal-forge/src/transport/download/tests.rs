use std::path::PathBuf;

use super::*;
use crate::account::{AgentLogins, Launch};
use crate::test_support::install_test_script;

/// Finds the CLIs only in one directory.
struct Bin(PathBuf);

impl Launch for Bin {
    fn apply(&self, command: &mut Command) {
        command.env("PATH", &self.0);
    }
}

#[tokio::test]
async fn a_cli_that_fails_after_a_successful_head_cut_the_file_off() {
    let dir = tempfile::tempdir().unwrap();
    install_test_script(
        &dir.path().join("gh"),
        "#!/bin/sh\nprintf 'HTTP/2.0 200 OK\\n\\nPN'\necho 'stream error' >&2\nexit 1\n",
    );
    let account = Account::agent(
        AgentLogins {
            launch: Box::new(Bin(dir.path().to_owned())),
            fj_home: None,
            fj: false,
            tea: false,
        },
        &Default::default(),
    );
    let error = through_cli(programs::GH, &account, &["https://github.com/a"])
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "gh failed: stream error");
}

#[test]
fn curl_output_splits_into_the_file_and_its_status() {
    let (body, status) = split_status(b"\x89PNG\n\0\xff\n200".to_vec()).unwrap();
    assert_eq!(body, b"\x89PNG\n\0\xff");
    assert_eq!(status, 200);
    let (body, status) = split_status(b"\n404".to_vec()).unwrap();
    assert!(body.is_empty());
    assert_eq!(status, 404);
    assert!(split_status(b"no status".to_vec()).is_err());
}
