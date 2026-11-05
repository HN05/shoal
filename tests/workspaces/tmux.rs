//! tmux as a terminal host, against an isolated tmux server each test starts
//! in its fixture; skipped where tmux is not installed. Test commands never
//! inherit the runner's tmux variables, and `TMUX_TMPDIR` puts the default
//! server's socket in the fixture, so nothing reaches the user's server.
use super::*;

/// The fixture's default tmux server, killed with the test, and the pane of
/// the session it started with, a shell the user opened.
struct TmuxServer {
    root: PathBuf,
    socket: String,
    /// `TMUX` as tmux sets it in the user's pane.
    tmux_var: String,
    session: String,
    users: String,
}

impl TmuxServer {
    fn start(fixture: &Fixture) -> Option<Self> {
        if !Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            eprintln!("skipped: tmux is not installed");
            return None;
        }
        let root = fixture.root.path().to_owned();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        if !bin.join("tmux").exists() {
            link_runner_program(&bin, "tmux");
        }
        // tmux makes its socket directory below `TMUX_TMPDIR`, not that.
        fs::create_dir_all(root.join("t")).unwrap();
        // The server's environment is every pane's: the fixture's tools,
        // and a plain shell.
        let output = support::isolated(&root, "tmux")
            .env("PATH", fixture.interactive_path())
            .env("SHELL", "/bin/sh")
            .current_dir(&fixture.repo)
            .args([
                "new-session",
                "-d",
                "-s",
                "caller",
                // A named window keeps its name instead of the command's.
                "-n",
                "mine",
                "-P",
                "-F",
                "#{pane_id} #{session_id} #{pid} #{socket_path}",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let started = String::from_utf8(output.stdout).unwrap();
        let fields: Vec<&str> = started.trim_end().splitn(4, ' ').collect();
        let [users, session, pid, socket] = fields[..] else {
            panic!("unexpected tmux output {started}");
        };
        Some(Self {
            tmux_var: format!("{socket},{pid},{}", session.trim_start_matches('$')),
            root,
            socket: socket.to_owned(),
            session: session.to_owned(),
            users: users.to_owned(),
        })
    }

    fn tmux(&self, args: &[&str]) -> Output {
        support::isolated(&self.root, "tmux")
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .output()
            .unwrap()
    }

    fn lines(&self, args: &[&str]) -> Vec<String> {
        let output = self.tmux(args);
        assert!(output.status.success(), "tmux {args:?}: {output:?}");
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// `command` as if run in the user's pane.
    fn inside<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command
            .env("TMUX", &self.tmux_var)
            .env("TMUX_PANE", &self.users)
    }

    /// The names of the windows of the user's session.
    fn windows(&self) -> Vec<String> {
        self.lines(&["list-windows", "-t", &self.session, "-F", "#{window_name}"])
    }

    /// Every pane on the server.
    fn panes(&self) -> Vec<String> {
        self.lines(&["list-panes", "-a", "-F", "#{pane_id}"])
    }

    fn pane_open(&self, pane: &str) -> bool {
        self.panes().iter().any(|open| open == pane)
    }

    /// The server's process and start time, which tell its panes apart from
    /// those of a later server.
    fn instance(&self) -> String {
        self.lines(&["display", "-p", "-t", &self.users, "#{pid}:#{start_time}"])[0].clone()
    }

    /// Type `args` for this test's Shoal into `pane`, as a user would.
    fn type_shoal(&self, fixture: &Fixture, pane: &str, args: &str) {
        let line = format!(
            " '{}' --state-dir '{}' {args}",
            env!("CARGO_BIN_EXE_shoal"),
            fixture.root.path().join("state").display()
        );
        let output = self.tmux(&["send-keys", "-t", pane, "-l", "--", &line]);
        assert!(output.status.success(), "{output:?}");
        let output = self.tmux(&["send-keys", "-t", pane, "Enter"]);
        assert!(output.status.success(), "{output:?}");
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
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

const AGENT: &str = "[agents.pi]\ncommand = ['sh', '-c', 'echo agent ran; exec sleep 600']\n";

/// Wait until `workspace` finished setup with one execution, its agent's.
fn wait_for_agent(fixture: &Fixture, workspace: &str) {
    wait_until("setup to finish with the agent running", || {
        fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .any(|listed| listed["name"] == workspace)
            && executions(fixture, workspace) == 1
            && fixture.ok(&["status", workspace])["setup_finished"] == true
    });
}

#[test]
fn additions_open_owned_windows_that_removal_closes_leaving_the_users_panes() {
    let fixture = Fixture::with_config(Some(&format!(
        "machine.max_agents = 'none'\n[terminal.tmux]\nenabled = true\n{AGENT}"
    )));
    let Some(tmux) = TmuxServer::start(&fixture) else {
        return;
    };
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = fixture.interactive_command(
        tmux.inside(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .args(["start", repo, "placed", "--agent", "pi"]),
        ),
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    wait_for_agent(&fixture, "placed");
    assert!(tmux.windows().contains(&"placed".to_owned()));
    assert_eq!(sessions(&fixture, "placed"), ["placed"]);

    // An agent in the user's own pane is recorded where it runs, but the
    // pane stays the user's.
    tmux.type_shoal(&fixture, &tmux.users, "run --workspace placed -- sleep 600");
    wait_until("the user's command to run", || {
        executions(&fixture, "placed") == 2
    });
    fixture.ok(&["rm", "placed", "--yes"]);
    wait_until("the window Shoal opened to close", || {
        !tmux.windows().contains(&"placed".to_owned())
    });
    assert!(tmux.pane_open(&tmux.users));
}

#[test]
fn split_placement_opens_additions_beside_the_callers_pane() {
    let fixture = Fixture::with_config(Some(&format!(
        "machine.max_agents = 'none'\n[terminal.tmux]\nenabled = true\nplacement = 'split'\n{AGENT}"
    )));
    let Some(tmux) = TmuxServer::start(&fixture) else {
        return;
    };
    let windows = tmux.windows();
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = fixture.interactive_command(
        tmux.inside(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .args(["start", repo, "placed", "--agent", "pi"]),
        ),
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    wait_for_agent(&fixture, "placed");
    // The split leaves the user's window its name.
    assert_eq!(tmux.windows(), windows);
    assert_eq!(tmux.panes().len(), 2);

    fixture.ok(&["rm", "placed", "--yes"]);
    wait_until("the split Shoal opened to close", || {
        tmux.panes().len() == 1
    });
    assert!(tmux.pane_open(&tmux.users));
    assert_eq!(tmux.windows(), windows);
}

#[test]
fn tmux_off_leaves_launches_from_its_panes_to_zmx() {
    // The agent exits at once, so the attached zmx session ends.
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    let Some(tmux) = TmuxServer::start(&fixture) else {
        return;
    };
    fixture.add("plain");
    let (output, transcript) = fixture.terminal_command(
        tmux.inside(fixture.command().args(["pi", "plain"])),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    let zmx_calls = fs::read_to_string(fixture.root.path().join("zmx-calls")).unwrap_or_default();
    assert!(zmx_calls.contains("attach"), "{zmx_calls} {transcript}");
    assert_eq!(tmux.panes(), std::slice::from_ref(&tmux.users));
}

#[test]
fn removal_closes_a_recorded_pane_only_on_the_server_that_created_it() {
    let fixture = Fixture::new();
    let Some(tmux) = TmuxServer::start(&fixture) else {
        return;
    };
    let record = |workspace: &str, pane: &str, instance: &str| {
        fixture.add(workspace);
        let recorded = fixture.request(serde_json::json!({"record_owned_place": {
            "workspace": workspace,
            "origin": {"host": "tmux", "place": pane, "owned": true,
                "data": {"socket": tmux.socket}, "instance": instance},
        }}));
        assert!(recorded.get("error").is_none(), "{recorded}");
    };
    let users = tmux.users.clone();
    let instance = tmux.instance();
    // A record from before the server restarted names another pane now
    // under the same ID.
    record("earlier", &users, "1:1");
    fixture.ok(&["rm", "earlier", "--yes"]);
    fixture.ok(&["cleanup"]);
    assert!(tmux.pane_open(&users));

    let opened = tmux.lines(&["split-window", "-d", "-P", "-F", "#{pane_id}"])[0].clone();
    record("current", &opened, &instance);
    fixture.ok(&["rm", "current", "--yes"]);
    wait_until("the recorded pane to close", || !tmux.pane_open(&opened));
    assert!(tmux.pane_open(&users));
}

#[test]
fn demos_open_in_the_configured_session_outside_tmux_and_take_input() {
    let fixture = Fixture::with_config(Some(
        "[terminal]\ndefault_host = 'tmux'\n[terminal.tmux]\nsession_name = 'demos'\n",
    ));
    let Some(tmux) = TmuxServer::start(&fixture) else {
        return;
    };
    fixture.add("tried");
    let demo = |args: &[&str]| {
        let output = fixture
            .command()
            .arg("--json")
            .arg("demo")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    demo(&[
        "start",
        "--workspace",
        "tried",
        "--",
        "sh",
        "-c",
        "echo ready; read line; echo \"got $line\"; exec sleep 600",
    ]);
    let sessions = tmux.lines(&["list-sessions", "-F", "#{session_name}"]);
    assert!(sessions.contains(&"demos".to_owned()), "{sessions:?}");
    let screen = || {
        demo(&["screen", "demo", "--workspace", "tried"])["screen"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    wait_until("the demo to run", || screen().contains("ready"));
    // Typing reaches the demo even while its pane shows copy mode.
    let panes = tmux.lines(&["list-panes", "-a", "-F", "#{pane_id} #{window_name}"]);
    let (pane, _) = panes
        .iter()
        .filter_map(|line| line.split_once(' '))
        .find(|(_, window)| window.contains("demo"))
        .unwrap_or_else(|| panic!("{panes:?}"));
    let output = tmux.tmux(&["copy-mode", "-t", pane]);
    assert!(output.status.success(), "{output:?}");
    demo(&["send", "demo", "--workspace", "tried", "hello;\r"]);
    wait_until("the demo to read its input", || {
        screen().contains("got hello;")
    });
    demo(&["stop", "--workspace", "tried"]);
    wait_until("the demo's window to close", || {
        !tmux
            .lines(&["list-windows", "-t", "demos", "-F", "#{window_name}"])
            .iter()
            .any(|window| window.contains("demo"))
    });
}
