//! Zellij as a terminal host, against an isolated Zellij session each test
//! starts in its fixture; skipped where Zellij is not installed. Test
//! commands never inherit the runner's Zellij variables, and a socket
//! directory of the test's own, short enough for Zellij's socket paths, keeps
//! everything from the user's sessions.
use std::{
    cell::RefCell,
    os::{fd::AsRawFd as _, unix::process::CommandExt as _},
    sync::{
        Arc, Once, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use super::*;

/// The user's session, `caller`, in a socket directory of the test's own,
/// with a client on a terminal of its own showing it, since Zellij drops tabs
/// opened in a session no client shows; every session there ends with the
/// test. A test that fails reports what Zellij showed of the session.
struct ZellijServer {
    root: PathBuf,
    sockets: TempDir,
    client: Child,
    /// The end of what the current client's terminal showed.
    screen: Arc<Mutex<Vec<u8>>>,
    /// Whether the client drew its session, which later output can push out
    /// of `screen`.
    drawn: Arc<AtomicBool>,
}

/// How much of the client's terminal output a failure reports.
const SCREEN_TAIL: usize = 4096;

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
        keep_panics();
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
            screen: Arc::default(),
            drawn: Arc::default(),
        };
        // The session opens its first tab after the command that creates it
        // returns, and a client that attaches before then gets no tab to
        // show. Under load its server can end before then: the session is
        // created again.
        let mut created = 0;
        let opened = try_wait_for(|| {
            if !server.sessions().iter().any(|session| session == "caller") {
                server.create(fixture);
                created += 1;
            }
            server
                .listed_panes("caller")
                .is_some_and(|panes| panes.iter().any(|(pane, _)| pane == USERS))
                .then_some(())
        });
        assert!(
            opened.is_some(),
            "timed out waiting for the user's session to open its pane; \
             sessions created: {created}"
        );
        server.client = server.attach("caller");
        server.show();
        Some(server)
    }

    /// Create the user's session in the background. Its environment is
    /// every pane's: the fixture's tools, and a plain shell.
    fn create(&self, fixture: &Fixture) {
        let output = self
            .zellij()
            .env("PATH", fixture.interactive_path())
            .env("SHELL", "/bin/sh")
            .current_dir(&fixture.repo)
            .args(["attach", "--create-background", "caller"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    /// Wait until the client shows the user's session, which Shoal opens
    /// tabs in only while a client shows it: Zellij lists the client, and
    /// the client drew the session. Under load Zellij can list a client
    /// that then exits, telling it the session is missing: another one
    /// attaches instead. A timeout reports what Zellij last listed and how
    /// often a client attached again.
    fn show(&mut self) {
        let mut listed = None;
        let mut reattached = 0;
        let shown = try_wait_for(|| {
            if let Some(status) = self.client.try_wait().unwrap() {
                eprintln!(
                    "the client exited: {status}; it showed:\n{}",
                    self.client_screen()
                );
                self.client = self.attach("caller");
                reattached += 1;
            }
            listed = self.clients("caller");
            (listed.as_ref().is_some_and(|clients| clients.len() == 1)
                && self.drawn.load(Ordering::Relaxed))
            .then_some(())
        });
        let drawn = self.drawn.load(Ordering::Relaxed);
        assert!(
            shown.is_some(),
            "timed out waiting for a client to show the user's session; \
             clients listed (None: session reported missing): {listed:?}; \
             the client drew the session: {drawn}; clients attached again: {reattached}"
        );
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

    /// A client attached to `session` on a terminal of its own, whose
    /// output a thread drains into a new `screen`, noting in a new `drawn`
    /// when it draws the session, so nothing an earlier client showed
    /// counts for it.
    fn attach(&mut self, session: &str) -> Child {
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
        self.screen = Arc::default();
        let screen = Arc::clone(&self.screen);
        self.drawn = Arc::default();
        let drawn = Arc::clone(&self.drawn);
        let title = format!("({session})");
        thread::spawn(move || drain(master, &screen, &drawn, &title));
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

    /// The running sessions. Zellij's own listing leaves out a session too
    /// busy to answer, so they are the sessions of the running servers.
    fn sessions(&self) -> Vec<String> {
        self.servers().unwrap()
    }

    /// The sessions whose server runs in the test's socket directory, as
    /// the servers' last argument, the session's socket, names them.
    fn servers(&self) -> std::io::Result<Vec<String>> {
        let output = Command::new("ps").args(["-A", "-o", "args="]).output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!("ps: {output:?}")));
        }
        let sockets = fs::canonicalize(self.sockets.path())?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (_, socket) = line.split_once(" --server ")?;
                let socket = Path::new(socket.trim_end());
                fs::canonicalize(socket.parent()?)
                    .is_ok_and(|dir| dir.starts_with(&sockets))
                    .then(|| socket.file_name()?.to_str().map(str::to_owned))?
            })
            .collect())
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
            support::shoal_binary(),
            fixture.root.path().join("state").display()
        );
        let mut write = self.zellij();
        write
            .args(["--session", "caller", "action", "write", "--pane-id", USERS])
            .args(line.bytes().map(|byte| byte.to_string()));
        let output = write.output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    /// What Zellij shows of the user's session: its client, its panes, the
    /// sessions, their servers and Zellij's log.
    fn report(&mut self) -> String {
        format!(
            "client: {}\nclient screen:\n{}\nlist-panes: {:?}\nlist-sessions: {:?}\nservers: {:?}\nzellij log:\n{}",
            self.client_state(),
            self.client_screen(),
            self.pane_table(),
            self.session_table(),
            self.servers(),
            zellij_log(),
        )
    }

    /// What `zellij action list-panes` prints of the user's session.
    fn pane_table(&self) -> std::io::Result<std::process::Output> {
        self.zellij()
            .args(["--session", "caller", "action", "list-panes", "--all"])
            .output()
    }

    /// What `zellij list-sessions` prints.
    fn session_table(&self) -> std::io::Result<std::process::Output> {
        self.zellij()
            .args(["list-sessions", "--no-formatting"])
            .output()
    }

    /// Whether the client still runs, or how it exited.
    fn client_state(&mut self) -> String {
        match self.client.try_wait() {
            Ok(Some(status)) => format!("exited: {status}"),
            Ok(None) => "running".to_owned(),
            Err(error) => error.to_string(),
        }
    }

    /// The text at the end of what the client's terminal showed.
    fn client_screen(&self) -> String {
        let screen = self.screen.lock().unwrap_or_else(PoisonError::into_inner);
        without_escapes(&String::from_utf8_lossy(&screen))
    }
}

thread_local! {
    /// What the test's panic said, which its failure report repeats.
    static PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Keep what each panic says for the failure report as well as printing it.
/// The report follows the panic's message and runs long, while CI keeps only
/// the end of a failed job's log: the report ends with the message again.
fn keep_panics() {
    static KEEP: Once = Once::new();
    KEEP.call_once(|| {
        let print = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANIC.set(Some(info.to_string()));
            print(info);
        }));
    });
}

impl Drop for ZellijServer {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!("{}", self.report());
            if let Some(panic) = PANIC.take() {
                eprintln!("the test {panic}");
            }
        }
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

/// Read what a client prints on the terminal `master` until it ends,
/// keeping it as [`keep_output`] does.
fn drain(mut master: fs::File, screen: &Mutex<Vec<u8>>, drawn: &AtomicBool, title: &str) {
    let mut buffer = [0; 4096];
    loop {
        match std::io::Read::read(&mut master, &mut buffer) {
            Ok(0) => break,
            Ok(count) => keep_output(screen, drawn, title, &buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

/// Add `output` to the end of `screen`, the client's, noting in `drawn`
/// once it shows `title`, before older output makes room for it.
fn keep_output(screen: &Mutex<Vec<u8>>, drawn: &AtomicBool, title: &str, output: &[u8]) {
    let mut screen = screen.lock().unwrap_or_else(PoisonError::into_inner);
    screen.extend_from_slice(output);
    if !drawn.load(Ordering::Relaxed)
        && without_escapes(&String::from_utf8_lossy(&screen)).contains(title)
    {
        drawn.store(true, Ordering::Relaxed);
    }
    let excess = screen.len().saturating_sub(SCREEN_TAIL);
    screen.drain(..excess);
}

/// `text` without its terminal escape sequences.
fn without_escapes(text: &str) -> String {
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(char) = chars.next() {
        if char != '\x1b' {
            plain.push(char);
            continue;
        }
        for char in chars.by_ref() {
            if char.is_ascii_alphabetic() || char == '\x07' {
                break;
            }
        }
    }
    plain
}

/// The end of Zellij's log, which every session of this user writes.
fn zellij_log() -> String {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let file = format!("zellij-{uid}/zellij-log/zellij.log");
    [PathBuf::from("/tmp"), std::env::temp_dir()]
        .iter()
        .find_map(|dir| fs::read_to_string(dir.join(&file)).ok())
        .map_or_else(
            || "none".to_owned(),
            |log| {
                let lines: Vec<_> = log.lines().collect();
                lines[lines.len().saturating_sub(60)..].join("\n")
            },
        )
}

/// The names of `workspace`'s sessions, as `shoal work status` lists them.
fn sessions(fixture: &Fixture, workspace: &str) -> Vec<String> {
    fixture.ok(&["work", "status", workspace])["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

fn executions(fixture: &Fixture, workspace: &str) -> usize {
    fixture.ok(&["work", "status", workspace])["executions"]
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
            && fixture.ok(&["work", "status", workspace])["setup_finished"] == true
    });
}

/// Add `workspace` with the agent from the user's pane, while a client
/// shows it.
fn add_inside(fixture: &Fixture, zellij: &mut ZellijServer, workspace: &str) {
    zellij.show();
    let repo = fixture.repo.to_str().unwrap();
    let (output, transcript) = fixture.interactive_command(
        zellij.inside(
            fixture
                .command()
                .current_dir(&fixture.repo)
                .args(["start", repo, workspace, "--agent", "pi"]),
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
    add_inside(&fixture, &mut zellij, "placed");
    // Where no client shows the user's session, Shoal opens one of its own.
    let status = fixture.ok(&["work", "status", "placed"]);
    assert_eq!(status["places"][0]["group"], "caller", "{status}");
    // A listing can still miss the tab Shoal saw open.
    wait_until("the tab Shoal opened to show", || {
        zellij.tabs().contains(&"placed".to_owned())
    });
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
    let Some(mut zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    let tabs = zellij.tabs();
    add_inside(&fixture, &mut zellij, "placed");
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
                        .args(["start", repo, "outside", "--agent", "pi"]),
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

/// The place Shoal recorded for `workspace`, with `pane` of the user's
/// session in its stead: on the same server instance.
fn place_beside(fixture: &Fixture, workspace: &str, pane: &str) -> Value {
    let mut place = fixture.ok(&["work", "status", workspace])["places"][0].clone();
    place["place"] = format!("caller:{pane}").into();
    place
}

impl ZellijServer {
    /// A new tab named `name` in the user's session, returning its pane.
    fn new_tab(&self, name: &str) -> String {
        let output = self
            .zellij()
            .args(["--session", "caller", "action", "new-tab", "--name", name])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        wait_for("the new tab to show", || {
            self.listed_panes("caller")?
                .into_iter()
                .find(|(_, tab)| tab == name)
                .map(|(pane, _)| pane)
        })
    }
}

#[test]
fn a_workspace_title_relabels_the_tab_shoal_opened_leaving_the_users() {
    let fixture = Fixture::with_config(Some(&format!(
        "machine.max_agents = 'none'\n[terminal.zellij]\nenabled = true\n{AGENT}"
    )));
    let Some(mut zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    add_inside(&fixture, &mut zellij, "placed");
    wait_until("the tab Shoal opened to show", || {
        zellij.tabs().contains(&"placed".to_owned())
    });
    let users_tab = zellij.tabs()[0].clone();
    // The user's own tab, recorded as the workspace's, shows the name the
    // user gave it.
    let recorded = fixture.request(serde_json::json!({"record_owned_place": {
        "workspace": "placed", "origin": place_beside(&fixture, "placed", USERS),
    }}));
    assert!(recorded.get("error").is_none(), "{recorded}");

    fixture.ok(&["work", "rename", "placed", "Login retries"]);
    assert_eq!(zellij.tabs(), [users_tab, "Login retries".to_owned()]);
}

#[test]
fn chat_rename_relabels_a_zellij_tab_that_still_shows_the_old_name() {
    let fixture = Fixture::with_config(Some(&format!(
        "machine.max_agents = 'none'\n[terminal.zellij]\nenabled = true\n{AGENT}"
    )));
    let Some(mut zellij) = ZellijServer::start(&fixture) else {
        return;
    };
    // A workspace's place records the server instance the chats' share.
    add_inside(&fixture, &mut zellij, "placed");
    let users_tab = zellij.tabs()[0].clone();
    let repository = fixture.ok(&["repo", "ls"])[0].clone();
    // The chats last while this test's process runs.
    let start = |name: &str, pane: &str| {
        let response = fixture.request(serde_json::json!({"start_chat":{
            "repository": repository["id"], "name": name, "agent": "claude",
            "pid": std::process::id(), "origin": place_beside(&fixture, "placed", pane),
        }}));
        assert_eq!(response["type"], "chat", "{response}");
    };
    // Shoal opened `talk`'s tab under its name; `notes` runs in the tab the
    // user named.
    start("talk", &zellij.new_tab("talk"));
    start("notes", USERS);

    fixture.ok(&["work", "rename", "talk", "topic"]);
    fixture.ok(&["work", "rename", "notes", "other"]);
    let tabs = zellij.tabs();
    assert!(tabs.contains(&"topic".to_owned()), "{tabs:?}");
    assert!(tabs.contains(&users_tab), "{tabs:?}");
    assert!(!tabs.contains(&"talk".to_owned()), "{tabs:?}");
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
