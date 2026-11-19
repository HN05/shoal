//! Attaching to an agent's session on another machine: that machine runs
//! Shoal's attach on a terminal of its own and carries it to the viewer.
use std::{fs, process::Stdio};

use shoal_client::attaching::attach;
use shoal_protocol::{
    attaching::{FromSession, TerminalSize, Viewer},
    space_names::SpaceSelector,
};

use super::{
    directory::{join, listed, machine, workspace},
    following::app,
    support::{
        fixture::Base,
        install_test_script, printed,
        prompt::{self, AnswerOptions},
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
        size: Some(TerminalSize {
            rows: 24,
            columns: 80,
        }),
        ..Viewer::default()
    }
}

#[test]
fn a_device_attaches_to_an_agent_s_session_on_another_machine() {
    let hub = machine(true, "app");
    let (laptop, space, mut agent) = laptop_agent(&hub);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (session, screen, code) = runtime.block_on(async {
        let (mut frames, _keys) = attach(
            &app(&hub),
            SpaceSelector::id(&space.as_str().into()),
            viewer(),
        )
        .await
        .unwrap();
        let (mut session, mut screen) = (None, Vec::new());
        loop {
            match frames
                .next()
                .await
                .unwrap()
                .expect("the attach's exit code")
            {
                FromSession::Session(chosen) => session = Some(chosen.name),
                FromSession::Screen(bytes) => screen.extend(bytes),
                FromSession::Exited(code) => return (session, screen, code),
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
