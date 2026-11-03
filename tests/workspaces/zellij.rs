//! Zellij as a terminal host, against an isolated Zellij session each test
//! starts in its fixture; skipped where Zellij is not installed. Test
//! commands never inherit the runner's Zellij variables, and a socket
//! directory of the test's own, short enough for Zellij's socket paths, keeps
//! everything from the user's sessions.
use std::os::{fd::AsRawFd as _, unix::process::CommandExt as _};

use super::*;

/// The user's session, `caller`, in a socket directory of the test's own,
/// with a client on a terminal of its own showing it, since Zellij drops tabs
/// opened in a session no client shows; every session there ends with the
/// test.
struct ZellijServer {
    root: PathBuf,
    sockets: TempDir,
    client: Child,
}

/// The pane the user's session started with, a shell the user opened.
const USERS: &str = "terminal_0";

impl ZellijServer {
    fn start(fixture: &Fixture) -> Option<Self> {
        if !Command::new("zellij")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            eprintln!("skipped: zellij is not installed");
            return None;
        }
        let root = fixture.root.path().to_owned();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        if !bin.join("zellij").exists() {
            link_runner_program(&bin, "zellij");
        }
        let config = root.join(".config/zellij");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("config.kdl"),
            "show_startup_tips false\nshow_release_notes false\nsession_serialization false\n",
        )
        .unwrap();
        let sockets = tempfile::Builder::new()
            .prefix("z")
            .tempdir_in("/tmp")
            .unwrap();
        let mut server = Self {
            root,
            sockets,
            client: Command::new("true").spawn().unwrap(),
        };
        // The session's environment is every pane's: the fixture's tools,
        // and a plain shell.
        let output = server
            .zellij()
            .env("PATH", fixture.interactive_path())
            .env("SHELL", "/bin/sh")
            .current_dir(&fixture.repo)
            .args(["attach", "--create-background", "caller"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        server.client = server.attach("caller");
        // Under load Zellij can tell a client attaching that the session is
        // missing, and the client exits: another one attaches instead.
        wait_until("a client to show the user's session", || {
            if server.client.try_wait().unwrap().is_some() {
                server.client = server.attach("caller");
            }
            server
                .clients("caller")
                .is_some_and(|clients| clients.len() == 1)
        });
        Some(server)
    }

    /// `command` reaching the test's sessions.
    fn reaching<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command.env("ZELLIJ_SOCKET_DIR", self.sockets.path())
    }

    fn zellij(&self) -> Command {
        let mut command = support::isolated(&self.root, "zellij");
        self.reaching(&mut command);
        command
    }

    /// A client attached to `session` on a terminal of its own.
    fn attach(&self, session: &str) -> Child {
        let (master, slave) = pty::open();
        let size = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the descriptor is the open master of a terminal.
        assert_eq!(
            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) },
            0
        );
        let mut attach = self.zellij();
        attach
            .env("TERM", "xterm-256color")
            .args(["attach", session])
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        // SAFETY: setsid and the terminal ioctl are async-signal-safe.
        unsafe {
            attach.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let client = attach.spawn().unwrap();
        thread::spawn(move || {
            let mut master = master;
            let mut buffer = [0; 4096];
            loop {
                match std::io::Read::read(&mut master, &mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        client
    }

    /// Leave the user's session, as detaching does.
    fn detach(&mut self) {
        let _ = self.client.kill();
        let _ = self.client.wait();
        wait_until("the client to leave", || {
            self.clients("caller")
                .is_some_and(|clients| clients.is_empty())
        });
    }

    /// The pane each client of `session` shows, and `None` while Zellij
    /// reports the session missing, as it can a busy one or one starting.
    fn clients(&self, session: &str) -> Option<Vec<String>> {
        let output = self
            .zellij()
            .args(["--session", session, "action", "list-clients"])
            .output()
            .unwrap();
        if reported_missing(&output) {
            return None;
        }
        assert!(output.status.success(), "zellij list-clients: {output:?}");
        let clients = String::from_utf8_lossy(&output.stdout)
            .lines()
            .skip(1)
            .filter_map(|line| line.split_whitespace().nth(1).map(str::to_owned))
            .collect();
        Some(clients)
    }

    /// Every terminal pane of `session`, which lasts the test, with its
    /// tab's name. Zellij can list nothing while it closes a tab, or report
    /// a busy session missing, which is asked again.
    fn panes(&self, session: &str) -> Vec<(String, String)> {
        wait_for("Zellij to list the panes", || self.listed_panes(session))
    }

    /// The terminal panes Zellij lists of `session`, and `None` when it
    /// lists nothing or reports the session missing.
    fn listed_panes(&self, session: &str) -> Option<Vec<(String, String)>> {
        let output = self
            .zellij()
            .args([
                "--session",
                session,
                "action",
                "list-panes",
                "--all",
                "--json",
            ])
            .output()
            .unwrap();
        if reported_missing(&output) || output.stdout.trim_ascii().is_empty() {
            return None;
        }
        assert!(output.status.success(), "zellij list-panes: {output:?}");
        let panes: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("zellij list-panes: {error}: {output:?}"));
        let panes = panes
            .as_array()
            .unwrap()
            .iter()
            .filter(|pane| pane["is_plugin"] == false)
            .map(|pane| {
                (
                    format!("terminal_{}", pane["id"]),
                    pane["tab_name"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        Some(panes)
    }

    fn pane_open(&self, session: &str, pane: &str) -> bool {
        self.panes(session).iter().any(|(open, _)| open == pane)
    }

    /// The names of the tabs of the user's session.
    fn tabs(&self) -> Vec<String> {
        let mut tabs: Vec<String> = self
            .panes("caller")
            .into_iter()
            .map(|(_, tab)| tab)
            .collect();
        tabs.dedup();
        tabs
    }

    /// The running sessions.
    fn sessions(&self) -> Vec<String> {
        let output = self
            .zellij()
            .args(["list-sessions", "--short", "--no-formatting"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// `command` as if run in the user's pane.
    fn inside<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        self.reaching(command)
            .env("ZELLIJ", "0")
            .env("ZELLIJ_SESSION_NAME", "caller")
            .env("ZELLIJ_PANE_ID", "0")
    }

    /// Type `args` for this test's Shoal into the user's pane, as a user
    /// would.
    fn type_shoal(&self, fixture: &Fixture, args: &str) {
        let line = format!(
            " '{}' --state-dir '{}' {args}\r",
            env!("CARGO_BIN_EXE_shoal"),
            fixture.root.path().join("state").display()
        );
        let mut write = self.zellij();
        write
            .args(["--session", "caller", "action", "write", "--pane-id", USERS])
            .args(line.bytes().map(|byte| byte.to_string()));
        let output = write.output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}

impl Drop for ZellijServer {
    fn drop(&mut self) {
        let _ = self.client.kill();
        let _ = self.client.wait();
        let _ = self.zellij().args(["kill-all-sessions", "--yes"]).output();
    }
}

/// Whether Zellij reported the session missing, as it can one busy or
/// starting; among several other sessions it lists them and succeeds.
fn reported_missing(output: &std::process::Output) -> bool {
    let error = String::from_utf8_lossy(&output.stderr);
    error.contains("There is no active session") || error.contains("' not found")
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

/// Add `workspace` with the agent from the user's pane.
fn add_inside(fixture: &Fixture, zellij: &ZellijServer, workspace: &str) {
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = fixture.interactive_command(
        zellij.inside(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .args(["add", repo, workspace, "--agent", "pi"]),
        ),
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    wait_for_agent(fixture, workspace);
}

#[test]
fn additions_open_owned_tabs_that_removal_closes_leaving_the_users_pane() {
    let fixture = Fixture::with_config(Some(&format!(
        "machine.max_agents = 'none'\n[terminal.zellij]\nenabled = true\n{AGENT}"
    )));
    let Some(mut zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    add_inside(&fixture, &zellij, "placed");
    assert!(zellij.tabs().contains(&"placed".to_owned()));
    assert_eq!(sessions(&fixture, "placed"), ["placed"]);

    // An agent in the user's own pane is recorded where it runs, but the
    // pane stays the user's.
    zellij.type_shoal(&fixture, "run --workspace placed -- sleep 600");
    wait_until("the user's command to run", || {
        executions(&fixture, "placed") == 2
    });
    // Agents outlive the client, and the tab closes without one.
    zellij.detach();
    assert_eq!(executions(&fixture, "placed"), 2);
    fixture.ok(&["rm", "placed", "--yes"]);
    wait_until("the tab Shoal opened to close", || {
        !zellij.tabs().contains(&"placed".to_owned())
    });
    assert!(zellij.pane_open("caller", USERS));
}

#[test]
fn split_placement_opens_additions_beside_the_callers_pane() {
    let fixture = Fixture::with_config(Some(&format!(
        "machine.max_agents = 'none'\n[terminal.zellij]\nenabled = true\nplacement = 'split'\n{AGENT}"
    )));
    let Some(zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    let tabs = zellij.tabs();
    add_inside(&fixture, &zellij, "placed");
    // The split leaves the user's tab its name.
    assert_eq!(zellij.tabs(), tabs);
    // A listing can still miss the pane Shoal saw open.
    wait_until("the split Shoal opened to show", || {
        zellij.panes("caller").len() == 2
    });

    fixture.ok(&["rm", "placed", "--yes"]);
    wait_until("the split Shoal opened to close", || {
        zellij.panes("caller").len() == 1
    });
    assert!(zellij.pane_open("caller", USERS));
}

#[test]
fn zellij_off_leaves_launches_from_its_panes_to_zmx() {
    // The agent exits at once, so the attached zmx session ends.
    let fixture = Fixture::with_config(Some(
        "machine.max_agents = 'none'\n[agents.pi]\ncommand = ['sh', '-c', 'exit 0']\n",
    ));
    let Some(zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    fixture.add("plain");
    let (output, transcript) = fixture.terminal_command(
        zellij.inside(fixture.command().args(["pi", "plain"])),
        "",
        true,
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    let zmx_calls = fs::read_to_string(fixture.root.path().join("zmx-calls")).unwrap_or_default();
    assert!(zmx_calls.contains("attach"), "{zmx_calls} {transcript}");
    assert_eq!(zellij.panes("caller").len(), 1);
}

#[test]
fn removal_leaves_a_pane_recorded_on_an_earlier_server() {
    let fixture = Fixture::new();
    let Some(zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    // A record from before the server restarted names another pane now
    // under the same ID; additions show a current record's pane closing.
    fixture.add("earlier");
    let recorded = fixture.request(serde_json::json!({"record_owned_place": {
        "workspace": "earlier",
        "origin": {"host": "zellij", "group": "caller", "place": format!("caller:{USERS}"),
            "owned": true, "data": {"socket_dir": zellij.sockets.path()}, "instance": "1:1"},
    }}));
    assert!(recorded.get("error").is_none(), "{recorded}");
    fixture.ok(&["rm", "earlier", "--yes"]);
    fixture.ok(&["cleanup"]);
    assert!(zellij.pane_open("caller", USERS));
}

#[test]
fn additions_outside_zellij_open_sessions_that_removal_ends() {
    let fixture = Fixture::with_config(Some(&format!(
        "machine.max_agents = 'none'\n[terminal]\ndefault_host = 'zellij'\n{AGENT}"
    )));
    let Some(zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    let repo = fixture.repo.to_str().unwrap();
    thread::scope(|scope| {
        // The launch attaches this terminal to the new session until it ends.
        let added = scope.spawn(|| {
            fixture.terminal_command(
                zellij.reaching(
                    fixture
                        .command()
                        .current_dir(&fixture.repo)
                        .args(["add", repo, "outside", "--agent", "pi"]),
                ),
                "",
                true,
            )
        });
        wait_for_agent(&fixture, "outside");
        assert!(zellij.sessions().contains(&"outside".to_owned()));
        assert_eq!(sessions(&fixture, "outside"), ["outside"]);
        fixture.ok(&["rm", "outside", "--yes"]);
        wait_until("the session Shoal opened to end", || {
            !zellij.sessions().contains(&"outside".to_owned())
        });
        let (_, transcript) = added.join().unwrap();
        assert!(transcript.contains("outside"), "{transcript}");
    });
    assert!(zellij.pane_open("caller", USERS));
}

#[test]
fn demos_open_in_sessions_of_their_own_outside_zellij_and_take_input() {
    let fixture = Fixture::with_config(Some("[terminal]\ndefault_host = 'zellij'\n"));
    let Some(zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    fixture.add("tried");
    let demo = |args: &[&str]| {
        let output = zellij
            .reaching(&mut fixture.command())
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
    let sessions = zellij.sessions();
    let own = sessions
        .iter()
        .find(|session| session.contains("demo"))
        .unwrap_or_else(|| panic!("{sessions:?}"))
        .clone();
    let screen = || {
        demo(&["screen", "demo", "--workspace", "tried"])["screen"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    wait_until("the demo to run", || screen().contains("ready"));
    demo(&["send", "demo", "--workspace", "tried", "hello;\r"]);
    wait_until("the demo to read its input", || {
        screen().contains("got hello;")
    });
    demo(&["stop", "--workspace", "tried"]);
    wait_until("the demo's session to end", || {
        !zellij.sessions().contains(&own)
    });
    assert!(zellij.pane_open("caller", USERS));
}
