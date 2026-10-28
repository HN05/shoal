use super::*;

fn fixture() -> Fixture {
    let fixture = Fixture::with_config(Some(
        "close_when_done = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
    ));
    install_test_script(
        fixture.root.path().join("bin/herdr"),
        include_str!("../fixtures/herdr_cleanup.py"),
    );
    fixture
}

fn start_workspace(fixture: &Fixture) -> anyhow::Result<Value> {
    let (caller, transcript) = herdr_call(
        fixture,
        &[
            "add",
            fixture
                .repo
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("repo path"))?,
            "owned",
            "--agent",
            "pi",
        ],
        "",
    );
    assert!(caller.status.success(), "{caller:?} {transcript}");
    let mut worker = herdr_worker_command(fixture);
    worker
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p9")
        .env(
            "HERDR_SOCKET_PATH",
            fixture.root.path().join("owned-herdr.sock"),
        );
    let (output, transcript) = fixture.interactive_command(&mut worker, "");
    assert!(output.status.success(), "{output:?} {transcript}");
    let directory = fixture.root.path().join("state/terminal-sessions/herdr");
    assert!(
        fs::read_dir(directory)?
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
    );
    Ok(fixture.ok(&["status", "owned"])["workspace"].clone())
}

fn closed(fixture: &Fixture) -> anyhow::Result<Vec<Value>> {
    let output = match fs::read_to_string(fixture.root.path().join("herdr-closed")) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(output
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}

#[test]
fn removal_closes_herdr_panes_with_the_recorded_server_and_preserves_replacements()
-> anyhow::Result<()> {
    for after in ["same", "replacement", "gone"] {
        let fixture = fixture();
        start_workspace(&fixture)?;
        if after == "replacement" {
            fs::write(
                fixture.root.path().join("herdr-terminal"),
                "replacement-terminal",
            )?;
        }
        if after == "gone" {
            fs::write(fixture.root.path().join("herdr-gone"), "")?;
        }
        fixture.ok(&["rm", "owned", "--yes"]);
        let expected = if after == "same" {
            vec![serde_json::json!([
                fixture.root.path().join("owned-herdr.sock"),
                "w1:p9"
            ])]
        } else {
            Vec::new()
        };
        assert_eq!(closed(&fixture)?, expected, "{after}");
    }
    Ok(())
}

#[test]
fn failed_herdr_closes_survive_removal_and_retry_at_startup() -> anyhow::Result<()> {
    let mut fixture = fixture();
    start_workspace(&fixture)?;
    fs::write(fixture.root.path().join("herdr-close-fails"), "")?;
    fixture.ok(&["rm", "owned", "--yes"]);
    assert!(closed(&fixture)?.is_empty());
    assert!(fixture.ok(&["work"]).as_array().is_some_and(Vec::is_empty));
    fs::remove_file(fixture.root.path().join("herdr-close-fails"))?;
    fixture
        .daemon
        .restart(fixture.root.path(), &mut daemon_launch(fixture.root.path()));
    wait_until("orphan Herdr pane cleanup", || {
        closed(&fixture).is_ok_and(|closed| closed.len() == 1)
    });
    Ok(())
}

#[test]
fn reusing_a_herdr_workspace_pane_for_a_chat_clears_cleanup_ownership() -> anyhow::Result<()> {
    for enabled in [true, false] {
        let fixture = fixture();
        start_workspace(&fixture)?;
        if !enabled {
            fs::write(
                fixture.root.path().join(".config/shoal/config.toml"),
                "close_when_done = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n[herdr]\nenabled = false\n",
            )?;
        }
        let mut command = fixture.command();
        command
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p9")
            .env(
                "HERDR_SOCKET_PATH",
                fixture.root.path().join("owned-herdr.sock"),
            )
            .args([
                "chat",
                fixture
                    .repo
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("repo path"))?,
                "--here",
                "--name",
                "chat",
                "--agent",
                "pi",
            ]);
        let (output, transcript) = fixture.interactive_command(&mut command, "");
        assert!(output.status.success(), "{output:?} {transcript}");
        fixture.ok(&["rm", "owned", "--yes"]);
        assert!(closed(&fixture)?.is_empty());
    }
    Ok(())
}

#[test]
fn failed_herdr_demo_handoffs_only_close_the_original_verified_pane() -> anyhow::Result<()> {
    for after in ["same", "replacement", "unreachable"] {
        let fixture = fixture();
        fixture.add("demo-owner");
        fs::write(fixture.root.path().join("herdr-run-fails"), after)?;
        let output = fixture
            .command()
            .env("HERDR_ENV", "1")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env(
                "HERDR_SOCKET_PATH",
                fixture.root.path().join("demo-herdr.sock"),
            )
            .args(["demo", "start", "--workspace", "demo-owner", "--", "true"])
            .output()?;
        assert!(!output.status.success(), "{after}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("handoff failed"),
            "{output:?}"
        );
        assert_eq!(
            closed(&fixture)?.len(),
            usize::from(after == "same"),
            "{after}"
        );
    }
    Ok(())
}

#[test]
fn removal_ends_the_place_watcher_without_closing_other_panes_in_its_tab() -> anyhow::Result<()> {
    let fixture = fixture();
    let workspace = start_workspace(&fixture)?;
    let id = workspace["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("workspace ID"))?;
    struct Watcher(Child);
    impl Drop for Watcher {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    // The mock keeps reporting an open tab after its owned pane closes,
    // representing another pane the user has split into that tab.
    let mut watcher = Watcher(
        fixture
            .command()
            .args(["internal", "herdr-watch", id, "w1:t9"])
            .spawn()?,
    );
    wait_until("place watcher host read", || {
        herdr_calls(&fixture)
            .iter()
            .any(|call| call == &["tab", "get", "w1:t9"])
    });
    fixture.ok(&["rm", "owned", "--yes"]);
    let status = wait_for("place watcher exit", || {
        watcher.0.try_wait().expect("query own watcher")
    });
    assert!(status.success());
    assert_eq!(closed(&fixture)?.len(), 1);
    assert!(
        !herdr_calls(&fixture)
            .iter()
            .any(|call| call == &["tab", "close", "w1:t9"])
    );
    Ok(())
}
