//! Following an agent's session on another machine: its machine streams the
//! bytes of the session's files through the hub as the agent's tool writes
//! them.
use std::{fs, io::Write as _, path::PathBuf, process::Stdio};

use shoal_client::{
    connection::{Connector, Daemon},
    transcripts::{Followed, SessionFollow, follow_session},
};
use shoal_protocol::space_names::SpaceSelector;

use super::{
    directory::{join, listed, machine, workspace},
    numbers::reroute,
    support::{
        fixture::{Base, home},
        printed, shoal_binary,
        wait::wait_for,
    },
};

/// A laptop's workspace whose agent reported the session `followed`, as
/// Claude Code's hooks do, with the path the laptop keeps its transcript
/// at.
struct Session {
    laptop: Base,
    laptop_id: String,
    space: String,
    transcript: PathBuf,
}

/// The hub and a laptop joined to it, with [`Session`] there that the hub
/// lists.
fn laptop_session(hub: &Base) -> Session {
    let laptop = Base::start(
        home(),
        "[agents.claude]\ncommand = ['sh', './agent.sh']\n",
        "app",
        &[],
    );
    let laptop_id = join(&laptop, hub, "laptop");
    let space = workspace(&laptop, "followed");
    let path = PathBuf::from(
        laptop.ok(&["status", &space])["workspace"]["path"]
            .as_str()
            .unwrap(),
    );
    fs::write(
        path.join("agent.sh"),
        format!(
            "printf '{{\"session_id\":\"followed\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n",
            shoal_binary()
        ),
    )
    .unwrap();
    let started = laptop.output(
        laptop
            .command()
            .args(["start", "--in", &space, "--here", "--agent", "claude"])
            .stdin(Stdio::null()),
    );
    assert!(started.status.success(), "{}", printed(&started));
    let transcript = laptop
        .root
        .path()
        .join(".claude/projects/-app/followed.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, first_turn()).unwrap();
    listed(hub, &space, &laptop_id);
    Session {
        laptop,
        laptop_id,
        space,
        transcript,
    }
}

/// The transcript's first line, longer than one frame carries, so that it
/// comes in several.
fn first_turn() -> Vec<u8> {
    format!("{{\"turn\":1,\"text\":\"{}\"}}\n", "x".repeat(100_000)).into_bytes()
}

/// Append the transcript's second line, as the agent's tool writes it.
fn second_turn(session: &Session) -> &'static [u8] {
    let line = b"{\"turn\":2}\n";
    let mut transcript = fs::OpenOptions::new()
        .append(true)
        .open(&session.transcript)
        .unwrap();
    transcript.write_all(line).unwrap();
    line
}

/// `machine`'s daemon as an app reaches it, as the user.
fn app(machine: &Base) -> Connector {
    let state = machine.root.path().join("state");
    Connector {
        paths: shoal_protocol::paths::Paths {
            home: machine.root.path().into(),
            socket: state.join("daemon.sock"),
            state,
        },
        scope: None,
    }
}

/// Add the transcript's bytes `follow` brings to `transcript`, until it
/// ends with `expected`.
async fn bytes_until<D: Daemon>(
    follow: &mut SessionFollow<D>,
    transcript: &mut Vec<u8>,
    expected: &[u8],
) {
    while !transcript.ends_with(expected) {
        match follow.next().await.unwrap() {
            Followed::Bytes {
                file,
                offset,
                bytes,
            } if file.ends_with("followed.jsonl") => {
                assert_eq!(offset, transcript.len() as u64);
                transcript.extend(bytes);
            }
            followed => panic!("expected the transcript's bytes, got {followed:?}"),
        }
    }
}

/// The session's ID as a request selects its space.
fn selected(session: &Session) -> SpaceSelector {
    SpaceSelector::id(&session.space.as_str().into())
}

/// Read the session `follow` brings first, which must be the one the agent
/// reported.
async fn followed_session<D: Daemon>(follow: &mut SessionFollow<D>) {
    let Followed::Session(followed) = follow.next().await.unwrap() else {
        panic!("expected the session first");
    };
    assert_eq!(followed.session.session.as_str(), "followed");
}

#[test]
fn a_machine_follows_a_session_on_a_third_machine_as_its_tool_writes_it() {
    let hub = machine(true, "app");
    let session = laptop_session(&hub);
    let desk = machine(false, "app");
    join(&desk, &hub, "desk");
    listed(&desk, &session.space, &session.laptop_id);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let mut follow = follow_session(app(&desk), selected(&session), None)
            .await
            .unwrap();
        followed_session(&mut follow).await;
        let mut followed = Vec::new();
        bytes_until(&mut follow, &mut followed, &first_turn()).await;
        let second = second_turn(&session);
        bytes_until(&mut follow, &mut followed, second).await;
        assert_eq!(followed, [first_turn(), second.to_vec()].concat());
    });
}

/// Follow `session` from `follower`, restart the laptop's daemon while
/// the follow reads, and check that it goes on with only what the
/// transcript gained since.
fn follows_on_across_a_restart_of_the_laptop(session: &mut Session, follower: Connector) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut follow = runtime.block_on(async {
        let mut follow = follow_session(follower, selected(session), None)
            .await
            .unwrap();
        followed_session(&mut follow).await;
        bytes_until(&mut follow, &mut Vec::new(), &first_turn()).await;
        follow
    });
    session.laptop.restart();
    let second = second_turn(session);
    let next = runtime.block_on(follow.next()).unwrap();
    let Followed::Bytes { offset, bytes, .. } = next else {
        panic!("expected only what the transcript gained, got {next:?}");
    };
    assert_eq!(
        (offset, bytes.as_slice()),
        (first_turn().len() as u64, second)
    );
}

#[test]
fn a_follow_goes_on_from_where_it_read_after_a_daemon_restart() {
    let hub = machine(true, "app");
    let mut session = laptop_session(&hub);
    let follower = app(&session.laptop);
    follows_on_across_a_restart_of_the_laptop(&mut session, follower);
}

#[test]
fn a_follow_through_the_hub_goes_on_once_the_restarted_machine_rejoins() {
    let hub = machine(true, "app");
    let mut session = laptop_session(&hub);
    let desk = machine(false, "app");
    join(&desk, &hub, "desk");
    listed(&desk, &session.space, &session.laptop_id);
    follows_on_across_a_restart_of_the_laptop(&mut session, app(&desk));
}

#[test]
fn a_follow_ends_once_its_scope_token_is_revoked() {
    let hub = machine(true, "app");
    let session = laptop_session(&hub);
    let issued = session
        .laptop
        .request(serde_json::json!({"issue_scope": {"space": session.space}}));
    let token = issued["data"].as_str().unwrap().to_owned();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let scoped = app(&session.laptop).scope(token.clone().into());
    let mut follow = runtime.block_on(async {
        let mut follow = follow_session(scoped, selected(&session), None)
            .await
            .unwrap();
        followed_session(&mut follow).await;
        bytes_until(&mut follow, &mut Vec::new(), &first_turn()).await;
        follow
    });
    let revoke = serde_json::json!({"revoke_scope": {"space": session.space, "token": token}});
    let revoked = session.laptop.request(revoke);
    assert_ne!(revoked["type"], "error", "{revoked}");
    let error = runtime.block_on(follow.next()).unwrap_err().to_string();
    assert!(error.contains("scope"), "{error}");
}

#[test]
fn following_a_session_on_an_offline_machine_fails_at_once_naming_it() {
    let hub = machine(true, "app");
    let session = laptop_session(&hub);
    reroute(&session.laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let refused = runtime.block_on(follow_session(app(&hub), selected(&session), None));
    let error = format!("{:#}", refused.err().expect("a refusal"));
    assert!(error.contains("machine laptop is offline"), "{error}");
}
