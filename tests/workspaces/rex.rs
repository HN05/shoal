//! Rex as a terminal host, against an isolated Rex server each test starts
//! in its fixture; skipped where Rex is not installed. Test commands never
//! inherit the runner's Rex variables, and every Rex call names the
//! isolated server, so nothing reaches the user's server.
use super::*;

/// Where the Mac app installs `rex`, below the home directory.
const APP_BIN_DIR: &str = "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS";

/// The runner's `rex`, found as Shoal finds it.
fn installed_rex() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::var_os("REX_BIN_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::split_paths(&path))
        .chain(std::env::var_os("HOME").map(|home| PathBuf::from(home).join(APP_BIN_DIR)));
    dirs.map(|dir| dir.join("rex")).find(|rex| rex.is_file())
}

/// A Rex server of the fixture's own, which stops with the test.
struct RexServer {
    child: Child,
    program: PathBuf,
    socket: PathBuf,
    root: PathBuf,
}

impl RexServer {
    fn start(fixture: &Fixture) -> Option<Self> {
        let Some(program) = installed_rex() else {
            eprintln!("skipped: Rex is not installed");
            return None;
        };
        let root = fixture.root.path().to_owned();
        let socket = root.join("rex.sock");
        fs::write(root.join("rex-init.lua"), "").unwrap();
        let child = support::isolated(&root, &program)
            .args(["server", "run", "--listen"])
            .arg(&socket)
            .arg("--state-dir")
            .arg(root.join("rex-state"))
            .arg("--config")
            .arg(root.join("rex-init.lua"))
            .arg("--exit-empty")
            // Blocks inherit it, so Shoal in them reaches this server.
            .env("REX_SERVER", &socket)
            .env("PATH", fixture.interactive_path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let server = Self {
            child,
            program,
            socket,
            root,
        };
        wait_until("the Rex server to start", || {
            server.rex(&["ls", "--json"]).status.success()
        });
        Some(server)
    }

    fn rex(&self, args: &[&str]) -> Output {
        support::isolated(&self.root, &self.program)
            .arg("--server")
            .arg(&self.socket)
            .arg("--autostart=false")
            .args(args)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.rex(args);
        assert!(output.status.success(), "rex {args:?}: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// A new session's ID and its first block, a shell the user opened.
    fn session(&self, label: &str, cwd: &Path) -> (String, String) {
        let created = self.json(&["new", label, "-c", cwd.to_str().unwrap(), "--json"]);
        (
            created["session_id"].as_str().unwrap().to_owned(),
            created["initial_windows"][0]["block_ids"][0]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    }

    /// `command` as if run in `block` of `session`.
    fn inside<'a>(&self, command: &'a mut Command, session: &str, block: &str) -> &'a mut Command {
        command
            .env("REX_SESSION", session)
            .env("REX_BLOCK", block)
            .env("REX_SERVER", &self.socket)
            .env("REX_BIN_DIR", self.program.parent().unwrap())
    }

    /// The labels of `session`'s windows.
    fn windows(&self, session: &str) -> Vec<String> {
        self.json(&["window", "ls", "-s", session, "--json"])["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|window| window["label"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    /// The labels of `session`'s blocks.
    fn blocks(&self, session: &str) -> Vec<String> {
        let input = serde_json::json!({"session_id": session}).to_string();
        self.json(&["api", "call", "session.list_blocks", &input])["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["label"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn block_open(&self, block: &str) -> bool {
        self.rex(&["block", "inspect", block, "--json"])
            .status
            .success()
    }

    /// Type `args` for this test's Shoal into `block`, as a user would.
    fn type_shoal(&self, fixture: &Fixture, block: &str, args: &str) {
        let line = format!(
            " '{}' --state-dir '{}' {args}\r",
            env!("CARGO_BIN_EXE_shoal"),
            fixture.root.path().join("state").display()
        );
        let output = self.rex(&["send", "-b", block, &line]);
        assert!(output.status.success(), "{output:?}");
    }
}

impl Drop for RexServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The names of `workspace`'s sessions, as `shoal status` lists them.
fn sessions(fixture: &Fixture, workspace: &str) -> Vec<String> {
    fixture.ok(&["status", workspace])["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn executions(fixture: &Fixture, workspace: &str) -> usize {
    fixture.ok(&["status", workspace])["executions"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn additions_open_owned_windows_that_removal_closes_leaving_the_users_blocks() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\n[commands]\npi = ['sh', '-c', 'echo agent ran; exec sleep 600']\n",
    ));
    let Some(rex) = RexServer::start(&fixture) else {
        return;
    };
    let (session, users) = rex.session("caller", &fixture.repo);
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = fixture.interactive_command(
        rex.inside(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .args(["add", repo, "placed", "--agent", "pi"]),
            &session,
            &users,
        ),
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    // The window's shell runs the add worker, which runs the agent in place.
    // The wrapper records its place once its execution registers, so wait
    // for the session itself.
    wait_until("the agent to run in its window", || {
        fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "placed")
    });
    assert!(rex.windows(&session).contains(&"placed".to_owned()));
    // Every rex call above was a client that has since disconnected; the
    // agent keeps running in its block. Setup records its place too, so
    // only once it finished is the one execution the agent's.
    wait_until("setup to finish with the agent running", || {
        let status = fixture.ok(&["status", "placed"]);
        status["setup_finished"] == true
            && status["executions"]
                .as_array()
                .is_some_and(|runs| runs.len() == 1)
    });
    assert_eq!(sessions(&fixture, "placed"), ["placed"]);

    // An agent in the user's own block is recorded where it runs, but the
    // block stays the user's.
    rex.type_shoal(&fixture, &users, "run --workspace placed -- sleep 600");
    wait_until("the user's command to run", || {
        executions(&fixture, "placed") == 2
    });
    fixture.ok(&["rm", "placed", "--yes"]);
    wait_until("the window Shoal opened to close", || {
        !rex.windows(&session).contains(&"placed".to_owned())
    });
    assert!(rex.block_open(&users));
}

#[test]
fn split_placement_opens_additions_and_chats_beside_the_callers_block() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\n[rex]\nplacement = 'split'\n[commands]\npi = ['sh', '-c', 'echo agent ran; exec sleep 600']\n",
    ));
    let Some(rex) = RexServer::start(&fixture) else {
        return;
    };
    let (session, users) = rex.session("caller", &fixture.repo);
    let windows = rex.windows(&session);
    let repo = fixture.repo.to_str().unwrap();
    let launch = |args: &[&str]| {
        let (output, transcript) = fixture.interactive_command(
            rex.inside(
                fixture.command().current_dir(&fixture.repo).args(args),
                &session,
                &users,
            ),
            "",
        );
        assert!(output.status.success(), "{output:?} {transcript}");
    };
    launch(&["add", repo, "placed", "--agent", "pi"]);
    wait_until("setup to finish with the agent running", || {
        fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["name"] == "placed")
            && executions(&fixture, "placed") == 1
            && fixture.ok(&["status", "placed"])["setup_finished"] == true
    });
    // The split names itself, never the caller's window.
    assert_eq!(rex.windows(&session), windows);
    assert_eq!(sessions(&fixture, "placed"), ["placed"]);
    launch(&["chat", repo, "--name", "talk", "--agent", "pi"]);
    wait_until("the chat to open beside the caller", || {
        rex.blocks(&session).len() == 3
    });
    assert_eq!(rex.windows(&session), windows);

    fixture.ok(&["rm", "placed", "--yes"]);
    wait_until("the split Shoal opened to close", || {
        !rex.blocks(&session).contains(&"placed".to_owned())
    });
    assert!(rex.block_open(&users));
    assert_eq!(rex.windows(&session), windows);
}

#[test]
fn demos_fall_back_to_the_configured_session_and_take_input() {
    let fixture = Fixture::with_config(Some("[rex]\nsession_name = 'demos'\n"));
    let Some(rex) = RexServer::start(&fixture) else {
        return;
    };
    fixture.add("tried");
    let demo = |args: &[&str]| {
        let output = rex
            .inside(
                fixture.command().arg("--json").arg("demo").args(args),
                "session:gone",
                "block:gone",
            )
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    // The block that starts it is in a session that is gone, so the demo
    // opens in the configured session on the same server.
    demo(&[
        "start",
        "--workspace",
        "tried",
        "--",
        "sh",
        "-c",
        "echo ready; read line; echo \"got $line\"; exec sleep 600",
    ]);
    let sessions = rex.json(&["ls", "--json"]);
    let session = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["label"] == "demos")
        .unwrap_or_else(|| panic!("{sessions}"))["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let screen = || {
        demo(&["screen", "demo", "--workspace", "tried"])["screen"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    wait_until("the demo to run", || screen().contains("ready"));
    send_until_shown(
        "the demo to read its input",
        || {
            demo(&["send", "demo", "--workspace", "tried", "hello\r"]);
        },
        screen,
        "got hello",
    );
    demo(&["stop", "--workspace", "tried"]);
    wait_until("the demo's window to close", || {
        !rex.windows(&session)
            .iter()
            .any(|window| window.contains("demo"))
    });
}

/// Send keys until `screen` shows `expected`, again after each few seconds
/// without it: nothing on a screen proves its program has the terminal and
/// is reading, so the first keys may be lost, while later ones are harmless.
/// Times out with the last screen.
fn send_until_shown(
    what: &str,
    mut send: impl FnMut(),
    mut screen: impl FnMut() -> String,
    expected: &str,
) {
    let mut shown = String::new();
    let mut sent: Option<Instant> = None;
    let found = try_wait_for(|| {
        shown = screen();
        if shown.contains(expected) {
            return Some(());
        }
        if sent.is_none_or(|sent| sent.elapsed() >= Duration::from_secs(5)) {
            send();
            sent = Some(Instant::now());
        }
        None
    });
    assert!(found.is_some(), "timed out waiting for {what}:\n{shown}");
}

#[test]
fn a_workspace_whose_block_is_gone_has_no_session_to_attach() {
    let fixture = Fixture::new();
    let Some(rex) = RexServer::start(&fixture) else {
        return;
    };
    fixture.add("gone");
    let (session, block) = rex.session("work", &fixture.repo);
    rex.type_shoal(&fixture, &block, "run --workspace gone -- sleep 600");
    wait_until("the command's place to be recorded", || {
        sessions(&fixture, "gone").len() == 1
    });
    let attach = || {
        fixture.interactive_command(
            rex.inside(fixture.command().args(["attach", "gone"]), &session, &block),
            "",
        )
    };
    let (attached, transcript) = attach();
    assert!(attached.status.success(), "{attached:?} {transcript}");
    assert!(rex.block_open(&block));
    assert!(rex.rex(&["block", "close", &block]).status.success());
    let (refused, transcript) = attach();
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        transcript.contains("gone has no agent sessions"),
        "{transcript}"
    );
}

#[test]
fn a_zmx_session_started_from_a_rex_block_with_rex_off_leaves_the_block_alone() {
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\n[rex]\nenabled = false\n[commands]\npi = ['sh', '-c', 'exit 0']\n",
    ));
    let Some(rex) = RexServer::start(&fixture) else {
        return;
    };
    fixture.add("plain");
    fixture.add("owner");
    let (session, block) = rex.session("mine", &fixture.repo);
    // Shoal created the block for another workspace.
    let recorded = fixture.request(serde_json::json!({"record_owned_place": {
        "workspace": "owner",
        "origin": {"host": "rex", "place": block, "data": {
            "server": rex.socket.to_str().unwrap(),
            "bin_dir": rex.program.parent().unwrap().to_str().unwrap(),
        }},
    }}));
    assert!(recorded.get("error").is_none(), "{recorded}");
    // With Rex off, the agent runs in a zmx session, which must not take
    // the block's Rex variables for its own place.
    let (output, transcript) = fixture.terminal_command(
        rex.inside(fixture.command().args(["pi", "plain"]), &session, &block),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    let zmx_calls = fs::read_to_string(fixture.root.path().join("zmx-calls")).unwrap_or_default();
    assert!(zmx_calls.contains("attach"), "{zmx_calls} {transcript}");
    fixture.ok(&["rm", "plain", "--yes"]);
    fixture.ok(&["cleanup"]);
    assert!(rex.block_open(&block));
}
