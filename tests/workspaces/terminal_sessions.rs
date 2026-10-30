use super::*;

#[test]
fn removal_and_cleanup_leave_a_session_the_user_opened() -> anyhow::Result<()> {
    let fixture = Fixture::new();
    fixture.add("guest");
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    install_test_script(home.join("bin/claude"), "#!/bin/sh\n");
    fs::write(home.join("zmx-keep-sessions"), "")?;
    fs::write(home.join("zmx-sessions"), "name=mine\tpid=1\n")?;
    let output = fixture
        .command()
        .env("ZMX_SESSION", "mine")
        .args(["claude", "guest"])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    let sessions = fs::read_to_string(home.join("zmx-sessions"))?;
    assert!(sessions.contains("shoal.workspace="), "{sessions}");
    assert!(!sessions.contains("shoal.created="), "{sessions}");
    fixture.ok(&["rm", "guest", "--yes"]);
    fixture.ok(&["cleanup"]);
    assert!(!zmx_calls(&fixture).iter().any(|call| call[0] == "kill"));
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
        format!(
            "name=reused\tpid=1\tshoal.created=1\tshoal.workspace={old_id}\tshoal.demo=old-demo\n"
        ),
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
        vec!["resume", "new-owner", "--focus"],
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
fn zmx_reassignment_waits_for_another_states_verified_close() -> anyhow::Result<()> {
    let original = Fixture::new();
    original.add("old-owner");
    let home = original.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    install_test_script(home.join("bin/claude"), "#!/bin/sh\n");
    fs::write(home.join("zmx-keep-sessions"), "")?;
    fs::write(
        home.join("zmx-sessions"),
        "name=reused\tpid=1\tshoal.created=1\n",
    )?;
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

/// A restore the daemon starts in a detached session closes that session once
/// a completion that keeps the workspace has the restored agent finished.
#[test]
fn a_resumed_session_ends_once_a_kept_completion_resolves_its_agent() -> anyhow::Result<()> {
    let fixture = Fixture::with_config(Some(&format!(
        "[cleanup.done]\nenabled = false\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\nresume_command = ['{}', 'done']\n",
        env!("CARGO_BIN_EXE_shoal")
    )));
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    // The fake session outlives its agent, as one with a shell would.
    fs::write(home.join("zmx-keep-sessions"), "")?;
    let workspace = fixture.add("resumed");
    let id = workspace["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("workspace ID"))?;
    let state = home.join("state/workspaces").join(id);
    fs::create_dir_all(&state)?;
    fs::write(
        state.join("resumed-agent.recovery.json"),
        r#"{"agent":"pi"}"#,
    )?;
    fixture.ok(&["resume", "resumed"]);
    // The watcher has the daemon close the session Shoal recorded creating.
    let watched = || fs::read_to_string(home.join("zmx-calls")).unwrap_or_default();
    wait_until("the session's end", || {
        watched()
            .lines()
            .any(|call| call == r#"["kill", "resumed"]"#)
    });
    let sent = zmx_calls(&fixture)
        .into_iter()
        .find(|call| call.starts_with(&["send".into(), "resumed".into()]))
        .ok_or_else(|| anyhow::anyhow!("no restore was sent to the session"))?;
    assert!(
        sent[2].contains(" --opened-in zmx --close-in zmx "),
        "{sent:?}"
    );
    assert!(
        fixture.ok(&["status", "resumed"])["workspace"]["done"]
            .as_bool()
            .unwrap_or_default()
    );
    Ok(())
}

#[test]
fn removals_refuse_to_end_the_zmx_session_they_run_in() -> anyhow::Result<()> {
    let fixture = Fixture::new();
    let workspace = fixture.add("sessioned");
    fixture.add("other");
    let id = workspace["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("workspace ID"))?;
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    // The session Shoal created for the workspace, as its labels say.
    fs::write(
        home.join("zmx-sessions"),
        created_zmx_session(&fixture, "sessioned", id),
    )?;
    let in_session = |session: &str, args: &[&str]| {
        fixture
            .command()
            .env("ZMX_SESSION", session)
            .args(args)
            .output()
    };
    let export = in_session("sessioned", &["export", "sessioned", "-o", "-"])?;
    assert!(!export.status.success());
    assert!(
        String::from_utf8_lossy(&export.stderr).contains("closes when the export removes"),
        "{export:?}"
    );
    let removal = in_session("sessioned", &["rm", "sessioned", "other", "--yes"])?;
    assert!(!removal.status.success());
    assert!(
        String::from_utf8_lossy(&removal.stderr)
            .contains("this terminal closes with the workspace"),
        "{removal:?}"
    );
    assert_eq!(fixture.ok(&["work"]).as_array().map(Vec::len), Some(2));
    // Another session goes on with the batch.
    let removal = in_session("elsewhere", &["rm", "sessioned", "other", "--yes"])?;
    assert!(removal.status.success(), "{removal:?}");
    Ok(())
}

#[test]
fn removals_from_a_pane_split_off_a_recorded_pane_go_on() -> anyhow::Result<()> {
    let fixture = Fixture::with_config(Some("[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n"));
    install_fake_herdr(&fixture);
    let repo = fixture.repo.to_str().unwrap_or_default();
    let (caller, _) = herdr_call(&fixture, &["add", repo, "tabbed", "--agent", "pi"], "");
    assert!(caller.status.success());
    // The worker records the pane Shoal opened, which removal closes alone.
    let (worker, transcript) = fixture.interactive_command(&mut herdr_worker_command(&fixture), "");
    assert!(worker.status.success(), "{worker:?} {transcript}");
    let export = |pane: &str| {
        fixture
            .command()
            .args(["export", "tabbed", "-o", "-"])
            .env("HERDR_ENV", "1")
            .env("HERDR_TAB_ID", "w1:t9")
            .env("HERDR_PANE_ID", pane)
            .output()
    };
    let refused = export("w1:p9")?;
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("closes when the export removes"),
        "{refused:?}"
    );
    // A pane the user split off the tab stays open, and keeps its stream.
    let exported = export("w1:p9.2")?;
    assert!(exported.status.success(), "{exported:?}");
    assert!(exported.stdout.starts_with(b"shoal workspace 1\n"));
    Ok(())
}
