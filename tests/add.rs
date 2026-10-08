#[path = "support/prompt.rs"]
mod prompt;
#[path = "support/pty.rs"]
mod pty;
mod support;

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

#[test]
fn add_reuses_resolution_requests_without_reordering_failures() {
    let url = "https://forge.example/team/repo/issues/0";
    for (args, expected, diagnostic) in [
        (vec!["add", url], vec![], "issue number must be positive"),
        (
            vec!["add", "--issue", url, "--agent", "custom"],
            vec!["list_repositories", "layered_config test"],
            "issue number must be positive",
        ),
        (
            vec!["add", "0", "--repo", "test"],
            vec!["layered_config test", "list_repositories"],
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
            vec!["layered_config test", "create_workspace"],
            "creation reached",
        ),
    ] {
        let daemon = FakeDaemon::start();
        // Agent launches are refused before creation when the agent is missing.
        let bin = daemon.root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("codex"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            bin.join("codex"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
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
        let (status, transcript) = prompt::answer(
            daemon.command().args(args).args(["--agent", "missing"]),
            answer,
        );
        assert_eq!(daemon.methods(), expected, "{args:?}: {transcript}");
        assert!(!status.success(), "{args:?}: {transcript}");
        assert!(
            transcript.contains("Register https://other.example/team/repo with Shoal? [y/N]"),
            "{transcript}"
        );
        assert!(transcript.contains(diagnostic), "{transcript}");
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
                    .current_dir(root.path())
                    .env("HOME", root.path())
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let socket = root.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let repo_path = root.path().to_owned();
        let server = thread::spawn(move || {
            let mut methods = Vec::new();
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).unwrap() == 0
                    || line.trim().is_empty()
                {
                    break;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
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
                let mut reply = match method.as_str() {
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
                    _ => {
                        json!({"type": "error", "data": {"code": "test", "message": "creation reached"}})
                    }
                };
                methods.push(method);
                reply["protocol"] = request["protocol"].clone();
                reply["id"] = request["id"].clone();
                writeln!(stream, "{reply}").unwrap();
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
        let mut command = support::cli(self.root.path());
        command
            .args(["--state-dir", self.root.path().to_str().unwrap()])
            .current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    fn methods(self) -> Vec<String> {
        // Keep the sentinel peer alive while macOS sets its read timeout.
        let mut sentinel = UnixStream::connect(&self.socket).unwrap();
        writeln!(sentinel).unwrap();
        self.server.join().unwrap()
    }
}
