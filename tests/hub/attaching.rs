//! Attaching to an agent's session on another machine: that machine runs
//! Shoal's attach on a terminal of its own and carries it to the viewer.
use std::{fs, io::Read as _, os::fd::AsRawFd as _, process::Stdio};

use shoal_client::attaching::attach;
use shoal_protocol::{
    attaching::{FromSession, Terminal, TerminalSize, Viewer},
    space_names::SpaceSelector,
};

use super::{
    directory::{join, listed, machine, workspace},
    following::app,
    support::{
        fixture::Base,
        install_test_script, printed,
        prompt::{self, AnswerOptions},
        pty,
        wait::wait_for,
        zmx::PASSTHROUGH_ZMX,
    },
};

/// A laptop joined to `hub` whose new workspace's agent runs in the fake
/// zmx session `persist`, with the workspace's ID; the agent runs until the
/// returned process is killed.
fn laptop_agent(hub: &Base) -> (Base, String, std::process::Child) {
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, hub, "laptop");
    let bin = laptop.root.path().join("bin");
    install_test_script(bin.join("zmx"), PASSTHROUGH_ZMX);
    install_test_script(
        bin.join("claude"),
        "#!/bin/sh\ntrap 'exit 0' TERM\nwhile :; do sleep 1; done\n",
    );
    // Debug builds only open an existing ownership lock of the host.
    fs::write(laptop.root.path().join("zmx.shoal-ownership.lock"), "").unwrap();
    let space = workspace(&laptop, "attached");
    fs::write(
        laptop.root.path().join("zmx-sessions"),
        format!("  name=persist\tpid=1\tclients=0\tshoal.workspace={space}\n"),
    )
    .unwrap();
    let agent = laptop
        .command()
        .env("ZMX_SESSION", "persist")
        .args([
            "start", "--in", &space, "--here", "--now", "--agent", "claude",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for("the agent listed in its session", || {
        let agents = laptop.request(serde_json::json!("list_agents"));
        let agents = agents["data"].as_array().cloned().unwrap_or_default();
        agents
            .iter()
            .any(|agent| {
                agent["space_id"] == space.as_str() && agent["origin"]["place"] == "persist"
            })
            .then_some(())
    });
    listed(hub, &space, &laptop_id);
    (laptop, space, agent)
}

fn viewer() -> Viewer {
    Viewer {
        terminal: Terminal {
            size: Some(TerminalSize {
                rows: 24,
                columns: 80,
            }),
            ..Terminal::default()
        },
        ..Viewer::default()
    }
}

#[test]
fn a_device_attaches_to_an_agent_s_session_on_another_machine() {
    let hub = machine(true, "app");
    let (laptop, space, mut agent) = laptop_agent(&hub);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (session, screen, left, code) = runtime.block_on(async {
        let (mut frames, _keys) = attach(
            &app(&hub),
            SpaceSelector::id(&space.as_str().into()),
            viewer(),
        )
        .await
        .unwrap();
        let (mut session, mut screen, mut left) = (None, Vec::new(), None);
        loop {
            match frames
                .next()
                .await
                .unwrap()
                .expect("the attach's exit code")
            {
                FromSession::Session(chosen) => session = Some(chosen.name),
                FromSession::Screen(bytes) => screen.extend(bytes),
                FromSession::Left(open) => left = Some(open),
                FromSession::Exited(code) => return (session, screen, left, code),
            }
        }
    });
    let screen = String::from_utf8_lossy(&screen);
    assert_eq!(code, 0, "{screen}");
    assert_eq!(session.as_deref(), Some("persist"), "{screen}");
    let calls = fs::read_to_string(laptop.root.path().join("zmx-calls")).unwrap();
    assert!(calls.contains(r#"["attach", "persist"]"#), "{calls}");
    // The session outlives the attach, which says how to return.
    assert!(screen.contains("shoal attach"), "{screen}");
    assert_eq!(left, Some(true), "{screen}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn shoal_attach_shows_an_agent_s_session_on_another_machine_in_this_terminal() {
    let hub = machine(true, "app");
    let (laptop, space, mut agent) = laptop_agent(&hub);
    let options = AnswerOptions {
        controlling_terminal: true,
        kill_on_timeout: true,
        terminal_stdout: true,
    };
    let (output, transcript) = prompt::answer(hub.command().args(["attach", &space]), "", options);
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert!(transcript.contains("shoal attach"), "{transcript}");
    let calls = fs::read_to_string(laptop.root.path().join("zmx-calls")).unwrap();
    assert!(calls.contains(r#"["attach", "persist"]"#), "{calls}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

/// `shoal attach target` from `hub`'s user's terminal, with its transcript.
fn attach_from(hub: &Base, target: &str) -> (std::process::Output, String) {
    let options = AnswerOptions {
        controlling_terminal: true,
        kill_on_timeout: true,
        terminal_stdout: true,
    };
    prompt::answer(hub.command().args(["attach", target]), "", options)
}

#[test]
fn attach_previous_returns_to_a_session_left_running_on_another_machine() {
    let hub = machine(true, "app");
    let (laptop, space, mut agent) = laptop_agent(&hub);
    let sessions = laptop.root.path().join("zmx-sessions");
    let listed = fs::read_to_string(&sessions).unwrap();
    // A session that ends while attached is not one to return to.
    fs::write(laptop.root.path().join("zmx-attach-ends-session"), "").unwrap();
    let (output, transcript) = attach_from(&hub, &space);
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    fs::remove_file(laptop.root.path().join("zmx-attach-ends-session")).unwrap();
    fs::write(&sessions, &listed).unwrap();
    let (output, transcript) = attach_from(&hub, "-");
    assert!(
        !output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert!(transcript.contains("no previous space"), "{transcript}");
    let (output, transcript) = attach_from(&hub, &space);
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let calls = || fs::read_to_string(laptop.root.path().join("zmx-calls")).unwrap();
    let before = calls().matches(r#"["attach", "persist"]"#).count();
    let (output, transcript) = attach_from(&hub, "-");
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    assert_eq!(
        calls().matches(r#"["attach", "persist"]"#).count(),
        before + 1
    );
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn a_signal_ends_an_attach_to_another_machine_leaving_the_terminal_as_it_was() {
    let hub = machine(true, "app");
    let (laptop, space, mut agent) = laptop_agent(&hub);
    // The laptop's zmx stays attached until its terminal hangs up.
    let bin = laptop.root.path().join("bin");
    fs::rename(bin.join("zmx"), bin.join("zmx-passthrough")).unwrap();
    install_test_script(
        bin.join("zmx"),
        "#!/bin/sh\nif [ \"$1\" = attach ]; then echo attached; exec sleep 600; fi\n\
         exec \"$(dirname \"$0\")/zmx-passthrough\" \"$@\"\n",
    );
    let (mut master, slave) = pty::open();
    let mut viewer = (hub.command())
        .args(["attach", &space])
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .spawn()
        .unwrap();
    let mut shown = Vec::new();
    let mut read_shown = |shown: &mut Vec<u8>| {
        let mut bytes = [0; 4096];
        while let Ok(read @ 1..) = master.read(&mut bytes) {
            shown.extend_from_slice(&bytes[..read]);
        }
    };
    wait_for("the session shown", || {
        read_shown(&mut shown);
        String::from_utf8_lossy(&shown)
            .contains("attached")
            .then_some(())
    });
    // SAFETY: kill signals the viewer this test started.
    assert_eq!(unsafe { libc::kill(viewer.id() as i32, libc::SIGTERM) }, 0);
    let status = wait_for("the attach to end", || viewer.try_wait().unwrap());
    assert!(!status.success(), "{status}");
    read_shown(&mut shown);
    let shown = String::from_utf8_lossy(&shown);
    assert!(shown.contains("\x1b[?1000l"), "{shown:?}");
    let mut settings = std::mem::MaybeUninit::uninit();
    // SAFETY: tcgetattr initializes settings on success.
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), settings.as_mut_ptr()) },
        0
    );
    // SAFETY: tcgetattr succeeded.
    let settings = unsafe { settings.assume_init() };
    assert_ne!(
        settings.c_lflag & libc::ICANON,
        0,
        "the terminal stayed raw"
    );
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn a_space_s_process_is_refused_an_attach_on_another_machine() {
    let hub = machine(true, "app");
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, &hub, "laptop");
    let space = workspace(&laptop, "attached");
    listed(&hub, &space, &laptop_id);
    let own = workspace(&hub, "own");
    let issued = hub.request(serde_json::json!({"issue_scope": {"space": own}}));
    let token = issued["data"].as_str().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let daemon = app(&hub).scope(token.into());
    let refused = runtime.block_on(attach(
        &daemon,
        SpaceSelector::id(&space.as_str().into()),
        viewer(),
    ));
    let error = format!("{:#}", refused.err().expect("a refusal"));
    assert!(error.contains("outside scoped executions"), "{error}");
}
