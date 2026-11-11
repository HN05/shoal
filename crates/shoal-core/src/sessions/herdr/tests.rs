use std::{os::unix::process::ExitStatusExt, process::ExitStatus};

use super::*;

fn failed(stdout: &str, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(1 << 8),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

#[test]
fn only_herdr_reporting_what_a_command_named_missing_proves_it_gone() {
    let missing =
        r#"{"error":{"code":"tab_not_found","message":"tab w2:t9 not found"},"id":"cli:tab:get"}"#;
    assert!(not_found(&failed("", missing), "tab_not_found"));
    assert!(!not_found(&failed("", missing), "pane_not_found"));
    // Herdr prints its errors on stderr.
    assert!(!not_found(&failed(missing, ""), "tab_not_found"));
    let unreachable = r#"{"id":"cli:tab:get","error":{"code":"server_not_running","message":"no herdr server is running"}}"#;
    assert!(!not_found(&failed("", unreachable), "tab_not_found"));
    assert!(!not_found(&failed("", "connection reset"), "tab_not_found"));
}

#[test]
fn a_command_reaches_only_the_server_its_record_names() {
    let envs = |data: &[(&str, &str)]| {
        let data = data
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let command = command(&data);
        command
            .as_std()
            .get_envs()
            .map(|(name, value)| {
                let name = name.to_string_lossy().into_owned();
                (
                    name,
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    // A record naming its session alone leaves out a socket this process
    // inherited, which would take precedence.
    let session = envs(&[("HERDR_SESSION", "work")]);
    assert_eq!(session.get("HERDR_SOCKET_PATH"), Some(&None));
    assert_eq!(session.get("HERDR_SESSION"), Some(&Some("work".into())));
    let socket = envs(&[("HERDR_SOCKET_PATH", "/work.sock")]);
    assert_eq!(socket.get("HERDR_SESSION"), Some(&None));
    assert_eq!(
        socket.get("HERDR_SOCKET_PATH"),
        Some(&Some("/work.sock".into()))
    );
}

#[test]
fn a_server_is_named_by_the_variable_that_routes_to_it() {
    let data = |vars: &[(&str, &str)]| {
        vars.iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>()
    };
    let socket = server(&data(&[("HERDR_SOCKET_PATH", "/work.sock")]));
    assert_eq!(
        server(&data(&[
            ("HERDR_SOCKET_PATH", "/work.sock"),
            ("HERDR_SESSION", "work")
        ])),
        socket
    );
    assert_ne!(server(&data(&[("HERDR_SESSION", "work")])), socket);
    assert_eq!(server(&data(&[("HERDR_ENV", "1")])), "");
}
