mod support;

use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Notify,
    task::JoinSet,
};

fn workspace(index: usize) -> Value {
    json!({
        "id": index.to_string(), "repository_id": "repo",
        "name": format!("workspace-{index:02}"), "path": "/unused",
        "branch": "branch", "state": "ready"
    })
}

async fn read(stream: &mut UnixStream) -> Value {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await.unwrap();
    let request: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["scope"], "test-scope");
    request
}

async fn reply(stream: &mut UnixStream, request: &Value, kind: &str, data: Value) {
    let response = json!({
        "protocol": request["protocol"], "id": request["id"],
        "type": kind, "data": data
    });
    stream
        .write_all(format!("{response}\n").as_bytes())
        .await
        .unwrap();
}

async fn overview_responses(listener: UnixListener, noun: &str, count: usize) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let request = read(&mut stream).await;
    assert_eq!(request["method"], "list_workspaces");
    reply(
        &mut stream,
        &request,
        "workspaces",
        (0..count).map(workspace).collect(),
    )
    .await;
    let release_first = Arc::new(Notify::new());
    let mut responses = JoinSet::new();
    for _ in 0..count {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read(&mut stream).await;
        let method = format!("{noun}_overview");
        let index: usize = request["method"][&method]["workspace"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let release_first = release_first.clone();
        let noun = noun.to_owned();
        responses.spawn(async move {
            if index == 0 && count > 1 {
                // A later request outside the initial window must be admitted.
                release_first.notified().await;
            } else {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if index == 1 {
                reply(
                    &mut stream,
                    &request,
                    "error",
                    json!({"code": "operation_failed", "message": "controlled failure"}),
                )
                .await;
            } else {
                let data = if noun == "port" {
                    json!({"workspace": workspace(index), "reserved": [], "configured": {}, "on_conflict": "suggest"})
                } else {
                    json!({"pools": [], "leases": []})
                };
                reply(&mut stream, &request, &format!("{noun}_overview"), data).await;
            }
        });
    }
    release_first.notify_one();
    while let Some(result) = responses.join_next().await {
        result.unwrap();
    }
}

#[tokio::test]
async fn all_workspace_overviews_preserve_json_text_scope_and_exit_status() {
    for noun in ["port", "resource"] {
        for count in [1, 8, 32] {
            for json in [true, false] {
                let root = tempfile::tempdir_in("/tmp").unwrap();
                std::fs::create_dir(root.path().join("state")).unwrap();
                let listener = UnixListener::bind(root.path().join("state/daemon.sock")).unwrap();
                let mut command = support::cli(root.path());
                command.env("SHOAL_SCOPE_TOKEN", "test-scope");
                if json {
                    command.arg("--json");
                }
                // Exercise the list alias as well as the bare noun.
                command.arg(noun);
                if count == 8 {
                    command.arg("list");
                }
                command.arg("--all");
                let mut command = tokio::process::Command::from(command);
                command.kill_on_drop(true);
                let (output, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                    tokio::join!(command.output(), overview_responses(listener, noun, count))
                })
                .await
                .expect("overview collection stalled");
                let output = output.unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(i32::from(count > 1)),
                    "{output:?}"
                );
                assert!(output.stderr.is_empty(), "{output:?}");
                if json {
                    let values: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(values.len(), count);
                    for (index, value) in values.iter().enumerate() {
                        if index == 1 {
                            assert_eq!(value["workspace"]["id"], index.to_string());
                            assert_eq!(value["error"], "operation_failed: controlled failure");
                            assert_eq!(value.as_object().unwrap().len(), 2);
                        } else if noun == "port" {
                            assert_eq!(value["workspace"]["id"], index.to_string());
                            assert_eq!(value["reserved"], json!([]));
                        } else {
                            assert_eq!(value["workspace"]["id"], index.to_string());
                            assert_eq!(value["pools"], json!([]));
                            assert_eq!(value["leases"], json!([]));
                            assert_eq!(value.as_object().unwrap().len(), 3);
                        }
                    }
                } else {
                    let empty = if noun == "port" {
                        "No configured or reserved ports"
                    } else {
                        "No configured resources or leases"
                    };
                    let expected: String = (0..count)
                        .map(|index| {
                            if index == 1 {
                                format!(
                                    "workspace-{index:02}: operation_failed: controlled failure\n"
                                )
                            } else {
                                format!("workspace-{index:02}\n{empty}\n")
                            }
                        })
                        .collect();
                    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
                }
            }
        }
    }
}
