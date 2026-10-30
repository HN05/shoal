use super::*;
use crate::{
    config::repo::ConfigLayers,
    model::{Chat, OwnedPlace, RunningAgent},
    protocol::{
        self, Body, ChatMethod, ConfigTarget, ExecutionMethod, Method, Operation, Request, Response,
    },
    sessions::{HostName, Origin},
    state::WorkspaceState,
};
use std::{fs, os::unix::fs::PermissionsExt};
use tokio::net::UnixListener;

#[tokio::test(start_paused = true)]
async fn live_lookups_keep_the_interactive_budget() {
    let started = tokio::time::Instant::now();
    let error = live(std::future::pending::<Result<()>>())
        .await
        .unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_millis(500));
    assert_eq!(error.to_string(), "live completion lookup timed out");
    assert_eq!(live(async { Ok("candidate") }).await.unwrap(), "candidate");
    assert_eq!(
        live::<()>(async { anyhow::bail!("discovery failed") })
            .await
            .unwrap_err()
            .to_string(),
        "discovery failed"
    );
}

#[tokio::test]
async fn attach_candidates_follow_recorded_places() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    install_session_fixture(root.path())?;
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        crate::test_support::isolated_test(
            root.path(),
            "cli::completion::tests::attach_candidates_child",
        )
        .env("PATH", root.path().join("bin"))
        .output(),
    )
    .await
    .context("attach candidate discovery stalled")??;
    assert!(output.status.success(), "{output:?}");
    // One listing answers for every recorded place, and Herdr is asked only
    // where it is on.
    assert_eq!(fs::read_to_string(root.path().join("zmx-lists"))?, "list\n");
    assert_eq!(
        fs::read_to_string(root.path().join("herdr-calls"))?,
        "pane get w9:p2\n"
    );
    Ok(())
}

fn install_session_fixture(root: &std::path::Path) -> Result<()> {
    let bin = root.join("bin");
    fs::create_dir(&bin)?;
    // Only `retained` is open: the records of `gone` and `stale` outlive
    // their sessions.
    let script = bin.join("zmx");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$*\" = list ] || exit 1\necho list >> '{}'\nprintf '  name=retained\\tpid=3\\tclients=0\\n'\n",
            root.join("zmx-lists").display()
        ),
    )?;
    fs::set_permissions(script, fs::Permissions::from_mode(0o755))?;
    let herdr = bin.join("herdr");
    fs::write(
        &herdr,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\necho unavailable\nexit 1\n",
            root.join("herdr-calls").display()
        ),
    )?;
    fs::set_permissions(herdr, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[tokio::test]
#[ignore = "isolated environment launched by attach candidate regression"]
async fn attach_candidates_child() -> Result<()> {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return Ok(());
    }
    let root = std::env::current_dir()?;
    let paths = Paths::for_test(&root);
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let daemon = serve_attach_targets(listener, &root);
    let typed = Typed {
        state: Some(paths.state),
        ..Typed::default()
    };
    // Exercise the same discovery as Tab, without requiring two subprocesses
    // and daemon replies to finish within its 500 ms interactive deadline.
    let (candidates, served) = tokio::join!(
        typed.candidates(Target::Workspaces(Need::Session), ""),
        daemon
    );
    served?;
    let candidates = candidates?;
    let names: BTreeSet<_> = candidates.iter().map(|value| value.get_value()).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            OsStr::new("first"),
            OsStr::new("second"),
            OsStr::new("fourth"),
            OsStr::new("chat")
        ])
    );
    Ok(())
}

async fn serve_attach_targets(listener: UnixListener, root: &std::path::Path) -> Result<()> {
    // Four listings, then the settings of each of the five workspaces whose
    // recorded places are asked about.
    for _ in 0..9 {
        let (mut stream, _) = listener.accept().await?;
        let request: Request = protocol::read(&mut stream).await?;
        let body = if matches!(
            request.method,
            Method::Operation(Operation::Execution(ExecutionMethod::ListAgents))
        ) {
            // A connected agent counts as it is; executions whose wrapper
            // is gone count only while their session is open.
            Body::RunningAgents(
                [
                    ("w1", Some("claude"), "workspace-session"),
                    ("w3", None, "stale"),
                    ("w4", None, "retained"),
                ]
                .map(|(workspace, agent, session)| RunningAgent {
                    execution: format!("{workspace}-execution"),
                    workspace_id: workspace.into(),
                    agent: agent.map(Into::into),
                    restorable: agent.is_some(),
                    state: None,
                    interrupted: false,
                    origin: Some(Origin::created(HostName::Zmx, session.into())),
                    restarted_by: Vec::new(),
                    restart: None,
                })
                .into(),
            )
        } else if matches!(
            request.method,
            Method::Operation(Operation::Workspace(WorkspaceMethod::OwnedPlaces))
        ) {
            // `fifth` turned Herdr off, so its pane is never asked about;
            // `sixth`'s Herdr cannot answer, which leaves zmx's answers.
            Body::OwnedPlaces(
                [
                    ("w2", HostName::Zmx, "retained"),
                    ("w3", HostName::Zmx, "gone"),
                    ("w5", HostName::Herdr, "w9:p1"),
                    ("w6", HostName::Herdr, "w9:p2"),
                ]
                .map(|(workspace, host, place)| OwnedPlace {
                    workspace_id: workspace.into(),
                    origin: Origin::created(host, place.into()),
                })
                .into(),
            )
        } else if let Method::Operation(Operation::Workspace(WorkspaceMethod::LayeredConfig {
            target: ConfigTarget::Workspace(workspace),
        })) = &request.method
        {
            let mut layers = ConfigLayers::default();
            layers.saved_repository_config.terminal.herdr.enabled = Some(workspace != "w5");
            Body::LayeredConfig(Box::new(layers))
        } else if matches!(
            request.method,
            Method::Operation(Operation::Chat(ChatMethod::ListChats))
        ) {
            Body::Chats(vec![Chat {
                id: "c1".into(),
                repository_id: "repo".into(),
                name: "chat".into(),
                agent: "codex".into(),
                pid: 1,
                started_at: 0,
                origin: Some(Origin::created(HostName::Zmx, "chat-session".into())),
            }])
        } else {
            assert!(matches!(
                request.method,
                Method::Operation(Operation::Workspace(WorkspaceMethod::ListWorkspaces))
            ));
            Body::Workspaces(attach_workspaces(root))
        };
        protocol::write(&mut stream, &Response::new(request.id, body)).await?;
    }
    Ok(())
}

fn attach_workspaces(root: &std::path::Path) -> Vec<Workspace> {
    [
        ("w1", "first"),
        ("w2", "second"),
        ("w3", "third"),
        ("w4", "fourth"),
        ("w5", "fifth"),
        ("w6", "sixth"),
    ]
    .into_iter()
    .map(|(id, name)| {
        let mut workspace = Workspace::new_record(
            "repo".into(),
            name.into(),
            root.join(name),
            name.into(),
            WorkspaceState::Ready,
        );
        workspace.id = id.into();
        workspace
    })
    .collect()
}

#[test]
fn a_repositorys_own_agents_complete_as_agents() {
    let layers = ConfigLayers {
        worktree_file: crate::config::repo::parse("[agents.repo-only]\ncommand = ['repo-agent']\n")
            .unwrap(),
        ..Default::default()
    };
    let settings = layered_settings(&Config::default(), &layers);
    assert!(settings.providers.contains("repo-only"));
    assert!(settings.agents.iter().any(|agent| agent == "repo-only"));
    assert_eq!(settings.commands["repo-only"], ["repo-agent"]);
    let global = layered_settings(&Config::default(), &ConfigLayers::default());
    assert!(!global.providers.contains("repo-only"));
}
