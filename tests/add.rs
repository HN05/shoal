mod common;

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::Stdio,
    thread,
    time::Duration,
};

#[test]
fn add_reuses_resolution_requests_without_reordering_failures() {
    let url = "https://forge.example/team/repo/issues/0";
    for (args, expected, diagnostic) in [
        (
            vec!["issue", url],
            vec!["list_repositories", "layered_config"],
            "issue number must be positive",
        ),
        (
            vec!["add", "--issue", url, "--agent", "custom"],
            vec!["list_repositories", "layered_config"],
            "issue number must be positive",
        ),
        (
            vec!["issue", "0", "--repo", "test"],
            vec!["layered_config", "list_repositories"],
            "issue number must be positive",
        ),
        (
            vec!["issue", "0", "--repo", "test", "--agent", "missing"],
            vec!["layered_config"],
            "unknown agent",
        ),
        (
            vec!["issue", "https://other.example/team/repo/issues/0"],
            vec!["list_repositories"],
            "no registered repository",
        ),
        (
            vec!["add", "test", "new"],
            vec!["create_workspace"],
            "creation reached",
        ),
        (
            vec!["add", "test", "new", "--agent", "codex"],
            vec!["layered_config", "create_workspace"],
            "creation reached",
        ),
    ] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["remote", "add", "origin", "https://forge.example/team/repo"],
        ] {
            assert!(
                common::isolated("git")
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
                if BufReader::new(&stream).read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                let method = request["method"]
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
                    "layered_config" => {
                        assert_eq!(
                            request["method"]["layered_config"]["target"],
                            json!({"repository": "test"})
                        );
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
        let output = common::isolated(env!("CARGO_BIN_EXE_shoal"))
            .args(["--state-dir", root.path().to_str().unwrap(), "--json"])
            .args(&args)
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        drop(UnixStream::connect(socket).unwrap());
        assert_eq!(server.join().unwrap(), expected, "{args:?}: {output:?}");
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{args:?}: {output:?}"
        );
    }
}
