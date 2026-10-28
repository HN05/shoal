use std::os::unix::fs::PermissionsExt;

use super::*;

/// A fake `rex` that records its arguments and answers from `answers`, a
/// Python dict from the first arguments to (exit status, stdout).
fn fake(answers: &str) -> (tempfile::TempDir, Rex) {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("rex");
    std::fs::write(
        &program,
        format!(
            r#"#!/usr/bin/env python3
import json, sys
args = sys.argv[1:]
with open(sys.argv[0] + '.calls', 'a') as calls:
    calls.write(json.dumps(args) + '\n')
answers = {answers}
words = [arg for arg in args if not arg.startswith('--autostart') and arg not in ('--server', 'unix:///rex.sock')]
for prefix, (status, out) in answers.items():
    if tuple(words[:len(prefix)]) == prefix:
        print(out)
        sys.exit(status)
sys.exit(9)
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let rex = Rex {
        program,
        server: Some("unix:///rex.sock".into()),
    };
    (root, rex)
}

fn calls(root: &tempfile::TempDir) -> Vec<Vec<String>> {
    std::fs::read_to_string(root.path().join("rex.calls"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_oldest_session_with_the_label_is_reused() {
    let sessions: Sessions = serde_json::from_str(
        r#"{"sessions":[{"session_id":"session:02","label":"shoal"},{"session_id":"session:01","label":"shoal"},{"session_id":"session:00","label":"other"},{"session_id":"session:03"}]}"#,
    )
    .unwrap();
    assert_eq!(
        oldest_labelled(&sessions.sessions, "shoal"),
        Some("session:01")
    );
    assert_eq!(oldest_labelled(&sessions.sessions, "none"), None);
}

#[test]
fn only_the_shell_rex_started_in_the_foreground_is_at_its_prompt() {
    let idle: Processes = serde_json::from_str(
        r#"{"last_exit":null,"child":{"pid":2057,"name":"login"},"foreground":{"pid":3049,"name":"zsh"},"ancestors":[{"pid":2057,"name":"login"}]}"#,
    )
    .unwrap();
    assert!(idle.shell_in_foreground());
    let busy: Processes = serde_json::from_str(
        r#"{"last_exit":null,"child":{"pid":2057},"foreground":{"pid":10160,"name":"sleep"},"ancestors":[{"pid":3049},{"pid":2057}]}"#,
    )
    .unwrap();
    assert!(!busy.shell_in_foreground());
    let exited: Processes = serde_json::from_str(
        r#"{"last_exit":{"code":0},"child":{"pid":2057},"foreground":{"pid":2057},"ancestors":[]}"#,
    )
    .unwrap();
    assert!(!exited.shell_in_foreground());
}

#[test]
fn only_a_connected_mac_app_shows_selections() {
    let clients: Clients = serde_json::from_str(
        r#"{"clients":[{"client_id":"client:cli","info":{"kind":"cli","platform":"macos"},"connection_state":"connected"},{"client_id":"client:phone","info":{"kind":"app","platform":"ios"},"connection_state":"connected"},{"client_id":"client:gone","info":{"kind":"app","platform":"macos"},"connection_state":"disconnected"},{"client_id":"client:mac","info":{"kind":"app","platform":"macos"},"connection_state":"connected"}]}"#,
    )
    .unwrap();
    assert_eq!(clients.mac_app(), Some("client:mac"));
    let none: Clients = serde_json::from_str(r#"{"clients":[]}"#).unwrap();
    assert_eq!(none.mac_app(), None);
}

#[test]
fn origins_record_how_to_reach_the_server_again() {
    let (_root, rex) = fake("{}");
    let recorded = Rex::recorded(&rex.data()).unwrap();
    assert_eq!(recorded, rex);
}

#[tokio::test]
async fn older_api_revisions_are_refused_by_name() {
    let (_root, old) =
        fake(r#"{('version',): (0, '{"rex_version": "0.0.1", "api_revision": 6}')}"#);
    let error = old.ensure_supported().await.unwrap_err().to_string();
    assert!(
        error.contains("revision 6") && error.contains("revision 7"),
        "{error}"
    );
    let (_root, current) =
        fake(r#"{('version',): (0, '{"rex_version": "0.0.2", "api_revision": 7}')}"#);
    current.ensure_supported().await.unwrap();
}

#[tokio::test]
async fn a_missing_session_is_created_and_an_existing_one_reused() {
    let (root, rex) = fake(
        r#"{('ls',): (0, '{"sessions": []}'), ('session', 'new'): (0, '{"session_id": "session:new"}')}"#,
    );
    let cwd = Path::new("/repo");
    assert_eq!(rex.session("shoal", cwd).await.unwrap(), "session:new");
    assert!(calls(&root).iter().any(|call| call.ends_with(&[
        "session".into(),
        "new".into(),
        "shoal".into(),
        "-c".into(),
        "/repo".into(),
        "--json".into()
    ])));
    let (root, rex) = fake(
        r#"{('ls',): (0, '{"sessions": [{"session_id": "session:old", "label": "shoal"}]}')}"#,
    );
    assert_eq!(rex.session("shoal", cwd).await.unwrap(), "session:old");
    assert_eq!(calls(&root).len(), 1);
}

#[tokio::test]
async fn a_gone_block_reads_as_gone_and_is_already_closed() {
    let (root, rex) = fake(r#"{('block', 'inspect'): (3, 'not found')}"#);
    assert_eq!(rex.block("block:1").await.unwrap(), None);
    rex.close("block:1").await.unwrap();
    assert!(
        calls(&root)
            .iter()
            .all(|call| !call.contains(&"close".to_owned()))
    );
}

#[tokio::test]
async fn an_unreachable_server_is_an_error_not_a_gone_block() {
    let (_root, rex) = fake(r#"{('block', 'inspect'): (4, 'no Rex server is running')}"#);
    assert!(rex.block("block:1").await.is_err());
}

#[tokio::test]
async fn a_new_window_types_its_input_into_a_shell() {
    let (root, rex) = fake(
        r#"{('api', 'call', 'session.new_window'): (0, '{"window_id": "window:1", "block_ids": ["block:1"], "revision": 2}')}"#,
    );
    let opened = rex
        .open(&Window {
            session: "session:1",
            label: "shoal#1",
            cwd: Path::new("/work"),
            focus: false,
            input: Some(" shoal internal launch /tmp/f\r"),
        })
        .await
        .unwrap();
    assert_eq!(
        opened,
        Opened {
            window: "window:1".into(),
            block: "block:1".into()
        }
    );
    let call = calls(&root).pop().unwrap();
    let input: serde_json::Value = serde_json::from_str(call.last().unwrap()).unwrap();
    assert_eq!(input["focus"], false);
    assert_eq!(input["window_label"], "shoal#1");
    let options = &input["layout"]["block"]["options"];
    assert_eq!(options["cwd"], "/work");
    assert_eq!(options["initial_input"], " shoal internal launch /tmp/f\r");
}
