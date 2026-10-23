#[path = "support/prompt.rs"]
mod prompt;
#[path = "support/pty.rs"]
mod pty;
mod support;

use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
};

#[test]
fn add_reuses_resolution_requests_without_reordering_failures() {
    let url = "https://forge.example/team/repo/issues/0";
    for (args, expected, diagnostic) in [
        (vec!["add", url], vec![], "issue number must be positive"),
        (
            vec!["add", "--issue", url, "--agent", "custom"],
            vec!["list_repositories", "layered_config test", "forge_type"],
            "issue number must be positive",
        ),
        (
            vec!["add", "0", "--repo", "test"],
            vec!["layered_config test", "list_repositories", "forge_type"],
            "issue number must be positive",
        ),
        (
            vec!["add", "0", "--repo", "test", "--agent", "missing"],
            vec!["layered_config test"],
            "unknown agent",
        ),
        (
            vec!["add", "https://other.example/team/repo/issues/7"],
            vec!["list_repositories"],
            "shoal repo add https://other.example/team/repo",
        ),
        (
            vec![
                "add",
                "--issue",
                "https://other.example/team/repo/issues/7#comment",
            ],
            vec!["list_repositories"],
            "shoal repo add https://other.example/team/repo",
        ),
        (
            vec!["review", "http://other.example:3000/team/repo/pulls/7"],
            vec!["list_repositories"],
            "shoal repo add http://other.example:3000/team/repo",
        ),
        (
            vec!["add", "test", "new"],
            vec!["create_workspace"],
            "creation reached",
        ),
        (
            vec!["add", "test", "new", "--agent", "codex"],
            vec!["layered_config test", "queue_add", "create_workspace"],
            "creation reached",
        ),
        // Work to wait for is refused before the agent or any picker.
        (
            vec![
                "add", "test", "new", "--agent", "codex", "--after", "missing",
            ],
            vec!["list_repositories", "check_dependencies"],
            "neither a workspace nor a queued agent",
        ),
    ] {
        let daemon = FakeDaemon::start();
        // Agent launches are refused before creation when the agent is missing.
        let bin = daemon.root.path().join("bin");
        support::install_test_script(bin.join("codex"), "#!/bin/sh\n");
        let output = daemon
            .command()
            .arg("--json")
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(daemon.methods(), expected, "{args:?}: {output:?}");
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "command_failed");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains(diagnostic),
            "{args:?}: {output:?}"
        );
    }
}

#[test]
fn unregistered_repositories_are_registered_on_confirmation() {
    let registered = vec![
        "list_repositories",
        "register_repository https://other.example/team/repo",
        "check_creation registered",
        "layered_config registered",
    ];
    let url_args: &[&str] = &["add", "https://other.example/team/repo/issues/7"];
    let explicit_args: &[&str] = &["add", "0", "--repo", "https://other.example/team/repo"];
    for (args, answer, expected, diagnostic) in [
        (url_args, "y\n", registered.clone(), "unknown agent"),
        (
            url_args,
            "n\n",
            vec!["list_repositories"],
            "shoal repo add https://other.example/team/repo",
        ),
        (explicit_args, "y\n", registered, "unknown agent"),
        (
            explicit_args,
            "n\n",
            vec!["list_repositories"],
            "repository is not registered",
        ),
    ] {
        let daemon = FakeDaemon::start();
        let (output, transcript) = prompt::answer(
            daemon.command().args(args).args(["--agent", "missing"]),
            answer,
            prompt::AnswerOptions::default(),
        );
        assert_eq!(daemon.methods(), expected, "{args:?}: {transcript}");
        assert!(!output.status.success(), "{args:?}: {transcript}");
        assert!(
            transcript.contains("Register https://other.example/team/repo with Shoal? [y/N]"),
            "{transcript}"
        );
        assert!(transcript.contains(diagnostic), "{transcript}");
    }
}

#[test]
fn interactive_commands_refuse_their_arguments_before_prompting() {
    for (args, answer, expected, refusal) in [
        (
            &["add", "missing"][..],
            "",
            &["check_creation missing"][..],
            "repository is not registered",
        ),
        // The typed branch is checked again before the agent picker.
        (
            &["add", "test", "--base", "main"],
            "taken\n",
            &["check_creation test", "check_creation test taken"],
            "workspace name already exists: taken",
        ),
        (
            &["repo", "rename", "missing"],
            "",
            &["list_repositories"],
            "repository is not registered: missing",
        ),
    ] {
        let daemon = FakeDaemon::start();
        let (output, transcript) = prompt::answer(
            daemon.command().args(args),
            answer,
            prompt::AnswerOptions::default(),
        );
        assert_eq!(daemon.methods(), expected, "{args:?}: {transcript}");
        assert!(!output.status.success(), "{args:?}: {transcript}");
        assert!(transcript.contains(refusal), "{args:?}: {transcript}");
    }
}

/// A daemon socket in a checkout of forge.example/team/repo that records each
/// request's method and answers with one registered repository.
struct FakeDaemon {
    root: tempfile::TempDir,
    socket: PathBuf,
    server: thread::JoinHandle<Vec<String>>,
}

impl FakeDaemon {
    fn start() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["remote", "add", "origin", "https://forge.example/team/repo"],
        ] {
            assert!(
                support::isolated(root.path(), "git")
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let listener = support::fake_daemon::bind_fake_daemon(root.path());
        let socket = root.path().join("state/daemon.sock");
        let repo_path = root.path().to_owned();
        let server = thread::spawn(move || {
            let mut methods = Vec::new();
            loop {
                let (stream, _) = listener.accept().unwrap();
                let Some(request) = support::fake_daemon::read_request(&stream) else {
                    break;
                };
                let mut method = request["method"]
                    .as_str()
                    .unwrap_or_else(|| {
                        request["method"]
                            .as_object()
                            .unwrap()
                            .keys()
                            .next()
                            .unwrap()
                    })
                    .to_owned();
                let reply = match method.as_str() {
                    "list_repositories" => json!({"type": "repositories", "data": [{
                        "id": "test", "path": repo_path,
                        "source": "https://forge.example/team/repo", "last_used": 0
                    }]}),
                    "register_repository" => {
                        let source = &request["method"]["register_repository"]["source"];
                        method = format!("register_repository {}", source.as_str().unwrap());
                        json!({"type": "repository", "data": {
                            "id": "registered", "path": repo_path, "source": source,
                            "last_used": 0
                        }})
                    }
                    "layered_config" => {
                        let target = &request["method"]["layered_config"]["target"];
                        method =
                            format!("layered_config {}", target["repository"].as_str().unwrap());
                        json!({"type": "layered_config", "data": {
                            "worktree_file": {}, "saved_repository_config": {
                                "default_agent": "custom", "commands": {"custom": ["false"]}
                            }
                        }})
                    }
                    "check_creation" => {
                        let check = &request["method"]["check_creation"];
                        let repository = check["repository"].as_str().unwrap();
                        let branch = check["branch"].as_str();
                        method = format!("check_creation {repository}");
                        if let Some(branch) = branch {
                            method = format!("{method} {branch}");
                        }
                        let refusal = if !["test", "registered"].contains(&repository) {
                            Some("repository is not registered")
                        } else if branch == Some("taken") {
                            Some("workspace name already exists: taken")
                        } else {
                            None
                        };
                        match refusal {
                            None => json!({"type": "ok"}),
                            Some(message) => json!({"type": "error", "data": {
                                "code": "test", "message": message
                            }}),
                        }
                    }
                    "check_dependencies" => json!({"type": "error", "data": {
                        "code": "test", "message": "missing is neither a workspace nor a queued agent"
                    }}),
                    // The forge an issue's host serves, before its number is read.
                    "forge_type" => json!({"type": "forge_type", "data": "forgejo"}),
                    "queue_add" => json!({"type": "queue_admission", "data": {
                        "start": true, "entry": null, "waiting": 0
                    }}),
                    _ => {
                        json!({"type": "error", "data": {"code": "test", "message": "creation reached"}})
                    }
                };
                methods.push(method);
                support::fake_daemon::write_reply(&stream, &request, reply);
            }
            methods
        });
        Self {
            root,
            socket,
            server,
        }
    }

    fn command(&self) -> Command {
        support::cli(self.root.path())
    }

    fn methods(self) -> Vec<String> {
        // Keep the sentinel peer alive while macOS sets its read timeout.
        let mut sentinel = UnixStream::connect(&self.socket).unwrap();
        writeln!(sentinel).unwrap();
        self.server.join().unwrap()
    }
}
