use super::*;

fn fixture() -> Fixture {
    let fixture = Fixture::with_config(Some(
        "close_when_done = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
    ));
    install_test_script(
        fixture.root.path().join("bin/herdr"),
        include_str!("../fixtures/herdr_cleanup.py"),
    );
    for socket in ["owned-herdr.sock", "demo-herdr.sock", "fixture-herdr.sock"] {
        fs::write(
            fixture
                .root
                .path()
                .join(format!("{socket}.shoal-ownership.lock")),
            "",
        )
        .expect("prepare isolated terminal lock");
    }
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

#[test]
fn a_chat_in_another_state_invalidates_prior_herdr_workspace_ownership() -> anyhow::Result<()> {
    for enabled in [true, false] {
        let original = fixture();
        start_workspace(&original)?;
        let other = fixture();
        if !enabled {
            fs::write(
                other.root.path().join(".config/shoal/config.toml"),
                "close_when_done = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n[herdr]\nenabled = false\n",
            )?;
        }
        let mut command = other.command();
        command
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p9")
            .env(
                "HERDR_SOCKET_PATH",
                original.root.path().join("owned-herdr.sock"),
            )
            .args([
                "chat",
                other
                    .repo
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("repo path"))?,
                "--here",
                "--name",
                "chat",
                "--agent",
                "pi",
            ]);
        let (output, transcript) = other.interactive_command(&mut command, "");
        assert!(output.status.success(), "{output:?} {transcript}");
        // The old state's durable record still exists, but its native claim
        // has been invalidated by the other state's chat.
        assert!(
            fs::read_dir(original.root.path().join("state/terminal-sessions/herdr"))?
                .filter_map(Result::ok)
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        );
        original.ok(&["rm", "owned", "--yes"]);
        assert!(closed(&original)?.is_empty());
    }
    Ok(())
}

#[test]
fn another_states_workspace_replaces_the_native_herdr_claim() -> anyhow::Result<()> {
    let original = fixture();
    start_workspace(&original)?;
    let other = fixture();
    fs::write(
        other.root.path().join(".config/shoal/config.toml"),
        "close_when_done = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n[herdr]\nnew_tab = false\n",
    )?;
    let mut command = other.command();
    command
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p9")
        .env(
            "HERDR_SOCKET_PATH",
            original.root.path().join("owned-herdr.sock"),
        )
        .args([
            "add",
            other
                .repo
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("repo path"))?,
            "new-owner",
            "--agent",
            "pi",
        ]);
    let (output, transcript) = other.interactive_command(&mut command, "");
    assert!(output.status.success(), "{output:?} {transcript}");
    original.ok(&["rm", "owned", "--yes"]);
    assert!(closed(&original)?.is_empty());
    other.ok(&["rm", "new-owner", "--yes"]);
    assert_eq!(closed(&original)?.len(), 1);
    Ok(())
}

/// Keep a direct shortcut running until its test explicitly stops it or unwinds.
struct Shortcut<'a> {
    fixture: &'a Fixture,
    child: Child,
}

impl Drop for Shortcut<'_> {
    fn drop(&mut self) {
        let _ = self.fixture.run(&["stop", "new-owner", "--yes"]);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_shortcut<'a>(fixture: &'a Fixture, command: &mut Command) -> anyhow::Result<Shortcut<'a>> {
    install_test_script(
        fixture.root.path().join("bin/claude"),
        "#!/bin/sh\nexec /bin/sleep 600\n",
    );
    let shortcut = Shortcut {
        fixture,
        child: command
            .args(["claude", "new-owner"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    };
    wait_registered_execution(fixture, "new-owner");
    Ok(shortcut)
}

#[test]
fn an_agent_shortcut_replaces_another_states_herdr_ownership() -> anyhow::Result<()> {
    for enabled in [true, false] {
        let original = fixture();
        start_workspace(&original)?;
        let other = fixture();
        if !enabled {
            fs::write(
                other.root.path().join(".config/shoal/config.toml"),
                "[herdr]\nenabled = false\n",
            )?;
        }
        let workspace = other.add("new-owner");
        let mut command = other.command();
        command
            .current_dir(
                workspace["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("workspace path"))?,
            )
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p9")
            .env(
                "HERDR_SOCKET_PATH",
                original.root.path().join("owned-herdr.sock"),
            );
        let mut agent = start_shortcut(&other, &mut command)?;
        original.ok(&["rm", "owned", "--yes"]);
        assert!(closed(&original)?.is_empty());
        assert!(agent.child.try_wait()?.is_none());
        drop(agent);
        other.ok(&["rm", "new-owner", "--yes"]);
        assert_eq!(closed(&original)?.len(), usize::from(enabled));
    }
    Ok(())
}

#[test]
fn an_agent_shortcut_replaces_reused_zmx_ownership() -> anyhow::Result<()> {
    let fixture = Fixture::new();
    let old = fixture.add("old-owner");
    let workspace = fixture.add("new-owner");
    let old_id = old["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("old workspace ID"))?;
    let new_id = workspace["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("new workspace ID"))?;
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    fs::write(home.join("zmx-keep-sessions"), "")?;
    fs::write(
        home.join("zmx-sessions"),
        format!("name=reused\tpid=1\tshoal.workspace={old_id}\tshoal.demo=old-demo\n"),
    )?;
    let mut agent = start_shortcut(
        &fixture,
        fixture.command().env("ZMX_SESSION", "reused").current_dir(
            workspace["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("workspace path"))?,
        ),
    )?;
    let sessions = fs::read_to_string(home.join("zmx-sessions"))?;
    assert!(sessions.contains(&format!("shoal.workspace={new_id}")));
    assert!(!sessions.contains("shoal.demo="));
    fixture.ok(&["rm", "old-owner", "--yes"]);
    assert!(!zmx_calls(&fixture).iter().any(|call| call[0] == "kill"));
    assert!(agent.child.try_wait()?.is_none());
    drop(agent);
    fixture.ok(&["rm", "new-owner", "--yes"]);
    assert!(
        zmx_calls(&fixture)
            .iter()
            .any(|call| call == &["kill", "reused"])
    );
    Ok(())
}

fn assert_failed_labelling_refuses_launches(
    fixture: &Fixture,
    environment: impl Fn(&mut Command),
) -> anyhow::Result<()> {
    let workspace = fixture.add("new-owner");
    let state = fixture.root.path().join("state/workspaces").join(
        workspace["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("workspace ID"))?,
    );
    fs::create_dir_all(&state)?;
    let recovery = state.join("saved.recovery.json");
    let saved = r#"{"agent":"claude","session":"previous-session"}"#;
    fs::write(&recovery, saved)?;
    install_test_script(
        fixture.root.path().join("bin/claude"),
        "#!/bin/sh\ntouch \"$HOME/new-agent-started\"\n",
    );
    let repo = fixture
        .repo
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("repository path"))?;
    for args in [
        vec!["claude", "new-owner"],
        vec!["resume", "new-owner", "--now"],
        vec![
            "chat",
            "--here",
            repo,
            "--name",
            "blocked-chat",
            "--agent",
            "claude",
        ],
        vec!["add", repo, "blocked-add", "--here", "--agent", "claude"],
    ] {
        let mut command = fixture.command();
        command.args(&args);
        environment(&mut command);
        let (output, transcript) = fixture.interactive_command(&mut command, "");
        assert!(
            !output.status.success(),
            "{args:?}: {output:?} {transcript}"
        );
        assert!(
            transcript.contains("ownership update failed"),
            "{args:?}: {transcript}"
        );
        assert!(!fixture.root.path().join("new-agent-started").exists());
        assert_eq!(fs::read_to_string(&recovery)?, saved);
        assert_eq!(
            fixture.ok(&["status", "new-owner"])["executions"],
            serde_json::json!([])
        );
    }
    Ok(())
}

#[test]
fn failed_herdr_labelling_refuses_agent_chat_resume_and_add_launches() -> anyhow::Result<()> {
    for enabled in [true, false] {
        let fixture = fixture();
        start_workspace(&fixture)?;
        let home = fixture.root.path();
        let ownership = fs::read(home.join("herdr-ownership"))?;
        if !enabled {
            fs::write(
                home.join(".config/shoal/config.toml"),
                "[herdr]\nenabled = false\n",
            )?;
        }
        fs::write(home.join("herdr-label-fails"), "")?;
        assert_failed_labelling_refuses_launches(&fixture, |command| {
            command
                .env("HERDR_ENV", "1")
                .env("HERDR_PANE_ID", "w1:p9")
                .env("HERDR_SOCKET_PATH", home.join("owned-herdr.sock"));
        })?;
        assert_eq!(fs::read(home.join("herdr-ownership"))?, ownership);
        fs::remove_file(home.join("herdr-label-fails"))?;
        fixture.ok(&["rm", "owned", "--yes"]);
        assert_eq!(closed(&fixture)?.len(), 1);
    }
    Ok(())
}

#[test]
fn failed_zmx_labelling_refuses_agent_chat_resume_and_add_launches() -> anyhow::Result<()> {
    let fixture = Fixture::new();
    let old = fixture.add("old-owner");
    let old_id = old["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("workspace ID"))?;
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    let ownership = format!("name=reused\tpid=1\tshoal.workspace={old_id}\n");
    fs::write(home.join("zmx-sessions"), &ownership)?;
    fs::write(home.join("zmx-keep-sessions"), "")?;
    fs::write(home.join("zmx-label-fails"), "")?;
    assert_failed_labelling_refuses_launches(&fixture, |command| {
        command.env("ZMX_SESSION", "reused");
    })?;
    assert_eq!(fs::read_to_string(home.join("zmx-sessions"))?, ownership);
    assert!(!zmx_calls(&fixture).iter().any(|call| call[0] == "kill"));
    Ok(())
}

#[test]
fn failed_herdr_labelling_refuses_the_add_worker_before_starting_its_agent() -> anyhow::Result<()> {
    let fixture = fixture();
    start_workspace(&fixture)?;
    let home = fixture.root.path();
    let ownership = fs::read(home.join("herdr-ownership"))?;
    install_test_script(
        home.join("bin/claude"),
        "#!/bin/sh\ntouch \"$HOME/new-agent-started\"\n",
    );
    let (output, transcript) = herdr_call(
        &fixture,
        &[
            "add",
            fixture
                .repo
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("repo path"))?,
            "blocked-worker",
            "--agent",
            "claude",
        ],
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    fs::write(home.join("herdr-label-fails"), "")?;
    let mut worker = herdr_worker_command(&fixture);
    worker.env("HERDR_SOCKET_PATH", home.join("owned-herdr.sock"));
    let (output, transcript) = fixture.interactive_command(&mut worker, "");
    assert!(!output.status.success(), "{output:?} {transcript}");
    assert!(transcript.contains("ownership update failed"));
    assert!(!home.join("new-agent-started").exists());
    assert_eq!(fs::read(home.join("herdr-ownership"))?, ownership);
    Ok(())
}

/// Always release the fixture's native query before reaping our CLI.
struct PausedTerminalCall {
    child: Child,
    release: PathBuf,
}

impl Drop for PausedTerminalCall {
    fn drop(&mut self) {
        let _ = fs::write(&self.release, "");
        let _ = self.child.wait();
    }
}

#[test]
fn failed_zmx_handoffs_exclude_cross_state_reassignment() -> anyhow::Result<()> {
    for stage in ["set", "send"] {
        let original = Fixture::new();
        original.add("failed");
        let other = Fixture::new();
        other.add("new-owner");
        let home = original.root.path();
        for fixture in [&original, &other] {
            install_test_script(
                fixture.root.path().join("bin/zmx"),
                include_str!("../fixtures/zmx_handoff.py"),
            );
        }
        install_test_script(
            other.root.path().join("bin/claude"),
            "#!/bin/sh\ntouch \"$HOME/new-agent-started\"\n",
        );
        fs::write(home.join("handoff-pause-cleanup"), "")?;
        let mut handoff = PausedTerminalCall {
            child: original
                .command()
                .env("HANDOFF_FAIL", stage)
                .env("HANDOFF_AFTER", "same")
                .args(["demo", "start", "--workspace", "failed", "--", "true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
            release: home.join("handoff-cleanup-release"),
        };
        wait_until("failed handoff verification paused", || {
            home.join("handoff-cleanup-paused").exists()
        });
        let shared = fs::File::open(home.join("zmx.shoal-ownership.lock"))?;
        assert_eq!(
            fs2::FileExt::try_lock_exclusive(&shared)
                .expect_err("failed handoff must exclude reassignment")
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        let fields: Value = serde_json::from_slice(&fs::read(home.join("handoff-session.json"))?)?;
        let session = fields["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("session name"))?;
        let mut agent = Shortcut {
            fixture: &other,
            child: other
                .command()
                .env("ZMX_SESSION", session)
                .env("HANDOFF_TEST_ROOT", home)
                .env_remove("ZMX_DIR")
                .args(["claude", "new-owner"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        };
        wait_until("shortcut resolves the shared native namespace", || {
            home.join("handoff-version-called").exists()
        });
        assert!(!other.root.path().join("new-agent-started").exists());
        fs::write(&handoff.release, "")?;
        assert!(!handoff.child.wait()?.success());
        let status = wait_for("shortcut observes the closed session", || {
            agent.child.try_wait().expect("query own shortcut")
        });
        assert!(!status.success());
        assert!(!other.root.path().join("new-agent-started").exists());
        assert!(!home.join("handoff-session.json").exists());
    }
    Ok(())
}

#[test]
fn herdr_reassignment_waits_for_another_states_verified_close() -> anyhow::Result<()> {
    let original = fixture();
    start_workspace(&original)?;
    let home = original.root.path();
    let other = fixture();
    other.add("new-owner");
    install_test_script(
        other.root.path().join("bin/claude"),
        "#!/bin/sh\ntouch \"$HOME/new-agent-started\"\n",
    );
    fs::write(home.join("herdr-pause-owner-check"), "")?;
    fs::write(home.join("herdr-close-removes-pane"), "")?;
    let mut removal = PausedTerminalCall {
        child: original
            .command()
            .args(["rm", "owned", "--yes"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
        release: home.join("herdr-owner-check-release"),
    };
    wait_until("native ownership check paused", || {
        home.join("herdr-owner-check-paused").exists()
    });
    // The native response has captured the old marker. Verify cleanup holds
    // the shared lock while that response and its following close are pending.
    let shared = fs::File::open(home.join("owned-herdr.sock.shoal-ownership.lock"))?;
    assert_eq!(
        fs2::FileExt::try_lock_exclusive(&shared)
            .expect_err("verified close must exclude cross-state reassignment")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    let ownership = fs::read(home.join("herdr-ownership"))?;
    let mut agent = Shortcut {
        fixture: &other,
        child: other
            .command()
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p9")
            .env("HERDR_SOCKET_PATH", home.join("owned-herdr.sock"))
            .args(["claude", "new-owner"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    };
    let local = other
        .root
        .path()
        .join("state/terminal-sessions/herdr/ownership.lock");
    let waiting = wait_for("reassignment reached its ownership operation", || {
        if agent
            .child
            .try_wait()
            .expect("query own shortcut")
            .is_some()
        {
            return Some(false);
        }
        let file = fs::File::open(&local).ok()?;
        fs2::FileExt::try_lock_exclusive(&file)
            .err()
            .filter(|error| error.kind() == std::io::ErrorKind::WouldBlock)
            .map(|_| true)
    });
    assert!(waiting, "reassignment must wait until the close finishes");
    assert_eq!(fs::read(home.join("herdr-ownership"))?, ownership);
    assert!(!other.root.path().join("new-agent-started").exists());
    fs::write(&removal.release, "")?;
    assert!(removal.child.wait()?.success());
    let status = wait_for("reassignment observes the closed pane", || {
        agent.child.try_wait().expect("query own shortcut")
    });
    assert!(!status.success());
    assert!(!other.root.path().join("new-agent-started").exists());
    assert_eq!(closed(&original)?.len(), 1);
    Ok(())
}

#[test]
fn zmx_reassignment_waits_for_another_states_verified_close() -> anyhow::Result<()> {
    let original = Fixture::new();
    original.add("old-owner");
    let home = original.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    install_test_script(home.join("bin/claude"), "#!/bin/sh\n");
    fs::write(home.join("zmx-keep-sessions"), "")?;
    fs::write(home.join("zmx-sessions"), "name=reused\tpid=1\n")?;
    let output = original
        .command()
        .env("ZMX_SESSION", "reused")
        .args(["claude", "old-owner"])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let other = Fixture::new();
    other.add("new-owner");
    install_test_script(other.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    install_test_script(
        other.root.path().join("bin/claude"),
        "#!/bin/sh\ntouch \"$HOME/new-agent-started\"\n",
    );
    fs::write(home.join("zmx-pause-owner-check"), "")?;
    fs::write(home.join("zmx-close-removes-session"), "")?;
    let mut removal = PausedTerminalCall {
        child: original
            .command()
            .args(["rm", "old-owner", "--yes"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
        release: home.join("zmx-owner-check-release"),
    };
    wait_until("zmx ownership check paused under its lock", || {
        home.join("zmx-owner-check-paused").exists()
    });
    let shared = fs::File::open(home.join("zmx.shoal-ownership.lock"))?;
    assert_eq!(
        fs2::FileExt::try_lock_exclusive(&shared)
            .expect_err("verified close must exclude cross-state reassignment")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    let mut agent = Shortcut {
        fixture: &other,
        child: other
            .command()
            .env("ZMX_SESSION", "reused")
            .env("ZMX_TEST_ROOT", home)
            .env_remove("ZMX_DIR")
            .args(["claude", "new-owner"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    };
    let waiting = wait_for("reassignment resolves the same native namespace", || {
        if agent
            .child
            .try_wait()
            .expect("query own shortcut")
            .is_some()
        {
            return Some(false);
        }
        home.join("zmx-version-called").exists().then_some(true)
    });
    assert!(waiting);
    assert!(!other.root.path().join("new-agent-started").exists());
    fs::write(&removal.release, "")?;
    assert!(removal.child.wait()?.success());
    let status = wait_for("reassignment observes the closed session", || {
        agent.child.try_wait().expect("query own shortcut")
    });
    assert!(!status.success());
    assert!(!other.root.path().join("new-agent-started").exists());
    Ok(())
}
