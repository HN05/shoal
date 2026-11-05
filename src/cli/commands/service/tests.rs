use std::path::Path;

use super::*;

#[test]
fn an_installed_default_host_needs_no_note() {
    let hosts = TerminalHosts {
        installed: vec![HostName::Herdr, HostName::Zmx],
        default: Some(HostName::Zmx),
    };
    assert_eq!(hosts.missing_default(Path::new("config.toml")), None);
}

#[test]
fn a_missing_default_host_names_an_installed_one() {
    let hosts = TerminalHosts {
        installed: vec![HostName::Herdr],
        default: Some(HostName::Zmx),
    };
    let note = hosts
        .missing_default(Path::new("config.toml"))
        .unwrap_or_default();
    assert!(note.contains("\"herdr\""), "{note}");
}

#[test]
fn no_installed_host_suggests_installing_the_default() {
    let hosts = TerminalHosts {
        installed: Vec::new(),
        default: Some(HostName::Zmx),
    };
    let note = hosts
        .missing_default(Path::new("config.toml"))
        .unwrap_or_default();
    assert!(note.contains("zmx"), "{note}");
    // No installed host to suggest instead.
    assert!(!note.contains("default_host = \""), "{note}");
}

#[test]
fn status_names_failing_cleanup() {
    let status = crate::protocol::DaemonStatus {
        pid: 42,
        version: "1.0.0".into(),
        uptime_secs: 1,
        managed: true,
        unread_notifications: 0,
        access_stalls: Vec::new(),
        cleanup_error: Some("Automatic cleanup failed 2 consecutive passes".into()),
    };
    let message = status_message(&status);
    assert_eq!(
        message.lines().nth(1),
        Some("Automatic cleanup failed 2 consecutive passes"),
        "{message}"
    );
}
