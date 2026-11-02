use std::{
    io::Read as _,
    os::{fd::FromRawFd as _, unix::process::CommandExt as _},
    process::Stdio,
    sync::{Arc, Mutex, PoisonError},
};

use super::*;

#[test]
fn places_name_their_session_and_pane() {
    assert_eq!(
        parse_place("shoal: repo:terminal_3"),
        Some(PaneRef {
            session: "shoal: repo",
            pane: "terminal_3",
        })
    );
    assert_eq!(place_id("repo", "terminal_0"), "repo:terminal_0");
    assert_eq!(parse_place("repo:plugin_2"), None);
    assert_eq!(parse_place(":terminal_1"), None);
    assert_eq!(parse_place("terminal_1"), None);
    assert_eq!(pane_of_number("4").as_deref(), Some("terminal_4"));
    assert_eq!(pane_of_number("x"), None);
    assert_eq!(pane_number("terminal_4").unwrap(), "4");
    assert!(pane_number("plugin_4").is_err());
}

#[test]
fn servers_are_this_users_zellij_serving_the_session() {
    let line = " 501 14016 /opt/homebrew/bin/zellij --server /tmp/z s/contract_version_1/my repo";
    assert_eq!(
        parse_server(line, 501, "my repo"),
        Some(Server {
            pid: 14016,
            socket_dir: PathBuf::from("/tmp/z s"),
        })
    );
    assert_eq!(parse_server(line, 502, "my repo"), None);
    assert_eq!(parse_server(line, 501, "repo"), None);
    let client = " 501 14017 zellij attach my-repo";
    assert_eq!(parse_server(client, 501, "my-repo"), None);
    let other = " 501 14018 /bin/sh -c x --server /tmp/z/contract_version_1/repo";
    assert_eq!(parse_server(other, 501, "repo"), None);
}

#[test]
fn panes_list_terminals_without_plugins() {
    let listed = r#"[
        {"id": 0, "is_plugin": true, "tab_id": 0, "tab_name": "Tab #1", "plugin_url": "tab-bar"},
        {"id": 2, "is_plugin": false, "tab_id": 1, "tab_name": "work", "exited": true,
         "pane_command": "/bin/zsh -l", "pane_cwd": "/work"}
    ]"#;
    assert_eq!(
        parse_panes(listed).unwrap(),
        [Pane {
            id: "terminal_2".into(),
            tab: 1,
            tab_name: "work".into(),
            command: Some("/bin/zsh -l".into()),
            exited: true,
        }]
    );
    assert!(parse_panes("").unwrap().is_empty());
}

#[test]
fn the_users_plugins_are_panes_but_layout_bars_are_not() {
    let listed = r#"[
        {"id": 1, "is_plugin": true, "tab_id": 0, "plugin_url": "tab-bar"},
        {"id": 2, "is_plugin": true, "tab_id": 0, "plugin_url": "zellij:status-bar"},
        {"id": 0, "is_plugin": true, "tab_id": 0, "plugin_url": "zellij:link", "is_suppressed": true},
        {"id": 4, "is_plugin": true, "tab_id": 0, "plugin_url": "zellij:strider"},
        {"id": 3, "is_plugin": false, "tab_id": 0}
    ]"#;
    let panes = parse_panes(listed).unwrap();
    let ids: Vec<&str> = panes.iter().map(|pane| pane.id.as_str()).collect();
    assert_eq!(ids, ["plugin_4", "terminal_3"]);
    assert!(!panes[0].is_terminal() && panes[1].is_terminal());
    // The plugin keeps the tab from closing with the terminal.
    assert!(!alone_in_tab(&panes[1], &panes));
    assert!(alone_in_tab(&panes[1], &panes[1..]));
}

#[test]
fn clients_and_sessions_read_their_listings() {
    let clients = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n1         terminal_1     sleep 60\n2 terminal_0 N/A\n";
    assert_eq!(parse_clients(clients), ["terminal_1", "terminal_0"]);
    let sessions =
        "my repo [Created 2m ago] (EXITED - attach to resurrect)\nwork [Created 1s ago] \n";
    assert_eq!(parse_sessions(sessions), ["my repo", "work"]);
}

#[test]
fn session_names_are_free_short_and_without_slashes() {
    assert_eq!(free_session_name("repo: a/b", 20, &[]), "repo: a_b");
    let taken = ["work".to_owned(), "work-2".to_owned()];
    assert_eq!(free_session_name("work", 20, &taken), "work-3");
    let long = free_session_name("a very long repository: and a branch", 20, &[]);
    assert_eq!(long, "a very long reposito");
    assert_eq!(free_session_name("ééééééééééé", 20, &[]).len(), 20);
    assert_eq!(free_session_name("login-fix", 5, &[]), "login");
    let taken = ["login".to_owned()];
    assert_eq!(free_session_name("login-fix", 5, &taken), "log-2");
}

#[test]
fn session_names_leave_room_for_the_socket_path() {
    let default = Zellij { socket_dir: None };
    assert_eq!(default.name_room(), MAX_SESSION_LEN);
    let deep = Zellij {
        socket_dir: Some(PathBuf::from(format!("/{}", "d".repeat(75)))),
    };
    assert_eq!(deep.name_room(), 7);
}

#[test]
fn sessions_are_the_same_only_in_one_socket_directory() {
    let at = |dir: Option<&str>| Zellij {
        socket_dir: dir.map(PathBuf::from),
    };
    assert!(at(Some("/tmp/a")).same_sessions(&at(Some("/tmp/a"))));
    assert!(!at(Some("/tmp/a")).same_sessions(&at(Some("/tmp/b"))));
    assert!(!at(Some("/tmp/a")).same_sessions(&at(None)));
    assert!(at(None).same_sessions(&at(None)));
}

#[test]
fn screens_keep_their_last_lines() {
    assert_eq!(last_lines("one\ntwo\nthree\n\n\n", 2), "two\nthree\n");
    assert_eq!(last_lines("one\n", 5), "one\n");
}

#[test]
fn versions_read_major_and_minor() {
    assert_eq!(parse_version("zellij 0.45.1\n"), Some((0, 45)));
    assert_eq!(parse_version("zellij 0.44.3"), Some((0, 44)));
    assert!(parse_version("zellij 0.44.3").is_some_and(|version| version < OLDEST));
    assert!(parse_version("zellij 1.0.0").is_some_and(|version| version >= OLDEST));
    assert_eq!(parse_version("zellij"), None);
}

#[test]
fn a_missing_session_lists_nothing() {
    assert!(nothing_to_list("There is no active session!"));
    assert!(nothing_to_list("No active zellij sessions found."));
    assert!(nothing_to_list(
        "Session 'gone' not found. The following sessions are active:"
    ));
    assert!(!nothing_to_list("Permission denied"));
    let missing = "Session 'busy' not found. The following sessions are active:\n";
    assert_eq!(
        listed(&exited(1, "", missing)).ok(),
        Some(Reply::Unanswered)
    );
    assert_eq!(
        listed(&exited(0, "[]", "")).ok(),
        Some(Reply::Answered("[]".to_owned()))
    );
}

#[test]
fn a_session_without_an_answer_is_reported_missing() {
    let missing = "No session named \"busy\" found.\n  help: Did you mean `busy`?\n";
    assert_eq!(
        killed(&exited(1, missing, "")).ok(),
        Some(Reply::Unanswered)
    );
    assert_eq!(killed(&exited(0, "", "")).ok(), Some(Reply::Answered(())));
    let none = "No active zellij sessions found.";
    assert_eq!(killed(&exited(1, "", none)).ok(), Some(Reply::Answered(())));
    let failed = killed(&exited(1, "Permission denied\n", "")).map_err(|error| error.to_string());
    assert_eq!(failed, Err("zellij failed: Permission denied".to_owned()));
}

#[test]
fn a_pane_listing_without_panes_is_no_answer() {
    assert!(!lists_panes(""));
    assert!(!lists_panes("[]\n"));
    assert!(!lists_panes("[\n]"));
    assert!(lists_panes(
        r#"[{"id": 0, "is_plugin": false, "tab_id": 0}]"#
    ));
    assert!(lists_panes("not a listing"));
}

#[tokio::test(start_paused = true)]
async fn a_session_reported_missing_is_asked_again_while_its_server_runs() {
    let mut replies = vec![Reply::Unanswered, Reply::Unanswered, Reply::Answered("[]")].into_iter();
    let mut asked = 0;
    let answer = asked_while_running(
        || {
            asked += 1;
            std::future::ready(Ok(replies.next().unwrap_or(Reply::Unanswered)))
        },
        || async { Ok(true) },
    )
    .await;
    assert_eq!(answer.ok(), Some(Some("[]")));
    assert_eq!(asked, 3);

    let ended = asked_while_running(
        || async { Ok(Reply::<&str>::Unanswered) },
        || async { Ok(false) },
    )
    .await;
    assert_eq!(ended.ok(), Some(None));

    let busy = asked_while_running(
        || async { Ok(Reply::<&str>::Unanswered) },
        || async { Ok(true) },
    )
    .await;
    let busy = busy.map_err(|error| error.to_string());
    assert_eq!(busy, Err("the running session does not answer".to_owned()));
}

/// What a command that exited with `code`, printing `stdout` and `stderr`,
/// leaves.
fn exited(code: i32, stdout: &str, stderr: &str) -> std::process::Output {
    std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(code << 8),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

/// A Zellij session of its own, in a socket directory short enough for its
/// socket path, started without the runner's Zellij config, home or shell
/// and ended on drop, with a client on a terminal of its own showing it,
/// since Zellij drops tabs opened in a session no client shows; `None`
/// without Zellij on PATH, which these tests then skip. A test that fails
/// reports what Zellij showed of the session.
struct Isolated {
    dir: tempfile::TempDir,
    zellij: Zellij,
    session: String,
    client: std::process::Child,
    /// The end of what the client's terminal showed.
    screen: Arc<Mutex<Vec<u8>>>,
}

/// How much of the client's terminal output a failure reports.
const SCREEN_TAIL: usize = 4096;

impl Isolated {
    fn start() -> Option<Self> {
        let installed = std::process::Command::new(Tool::Zellij.program())
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !installed {
            eprintln!("skipping: zellij is not installed");
            return None;
        }
        let dir = tempfile::Builder::new()
            .prefix("z")
            .tempdir_in("/tmp")
            .unwrap();
        let config = dir.path().join(".config/zellij");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.kdl"),
            "show_startup_tips false\nshow_release_notes false\nsession_serialization false\n",
        )
        .unwrap();
        // Named after its directory, so no other server shares the name.
        let session = dir.path().file_name().unwrap().to_str().unwrap().to_owned();
        let started = Self::zellij_in(dir.path())
            .args(["attach", "--create-background", &session])
            .output()
            .unwrap();
        assert!(started.status.success(), "{started:?}");
        let screen = Arc::default();
        let client = Self::client(dir.path(), &session, Arc::clone(&screen));
        let zellij = Zellij {
            socket_dir: Some(dir.path().join("s")),
        };
        Some(Self {
            dir,
            zellij,
            session,
            client,
            screen,
        })
    }

    /// `zellij` with the session's environment, which is every pane's.
    fn zellij_in(dir: &Path) -> std::process::Command {
        let mut command = std::process::Command::new(Tool::Zellij.program());
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", dir)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env(SOCKET_DIR_VAR, dir.join("s"))
            .current_dir(dir)
            .stdin(Stdio::null());
        command
    }

    /// A client attached to `session` on a terminal of its own, whose
    /// output a thread drains into `screen`.
    fn client(dir: &Path, session: &str, screen: Arc<Mutex<Vec<u8>>>) -> std::process::Child {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initializes both descriptors, which the files own.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                // macOS takes the size mutably and Linux does not.
                &raw mut size,
            )
        };
        assert_eq!(opened, 0);
        let master = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        let mut attach = Self::zellij_in(dir);
        attach
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
        std::thread::spawn(move || {
            let mut master = master;
            let mut read = [0; 4096];
            while let Ok(count @ 1..) = master.read(&mut read) {
                let mut screen = screen.lock().unwrap_or_else(PoisonError::into_inner);
                screen.extend_from_slice(&read[..count]);
                let excess = screen.len().saturating_sub(SCREEN_TAIL);
                screen.drain(..excess);
            }
        });
        client
    }

    fn cwd(&self) -> &Path {
        self.dir.path()
    }

    fn shell<'a>(&'a self, command: &'a [String]) -> Shell<'a> {
        Shell {
            label: "placed",
            cwd: self.cwd(),
            focus: false,
            command,
        }
    }

    /// Wait until the client shows the session, which Zellij's tabs need.
    async fn shown(&mut self) {
        for _ in 0..600 {
            if !self.zellij.clients(&self.session).await.unwrap().is_empty() {
                return;
            }
            if let Some(status) = self.client.try_wait().unwrap() {
                panic!("the client of {} exited: {status}", self.session);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("no client attached to {}", self.session);
    }

    /// What Zellij shows of the session: its client, its listing, its
    /// socket, its server and Zellij's log.
    fn report(&mut self) -> String {
        let client = self.client_state();
        format!(
            "session {}\nclient: {}\nclient screen:\n{}\nlist-sessions: {}\nsockets: {:?}\nprocesses:\n{}\nzellij log:\n{}",
            self.session,
            client,
            self.client_screen(),
            self.listed(),
            walkdir(&self.dir.path().join("s")),
            self.processes(),
            zellij_log(),
        )
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

    /// What `zellij list-sessions` prints in the session's socket directory.
    fn listed(&self) -> String {
        Self::zellij_in(self.dir.path())
            .args(["list-sessions", "--no-formatting"])
            .output()
            .map_or_else(|error| error.to_string(), |output| format!("{output:?}"))
    }

    /// The processes naming the session or its directory, with their
    /// parents and states.
    fn processes(&self) -> String {
        let listed = std::process::Command::new(Tool::Ps.program())
            .args(["-A", "-o", "pid=,ppid=,stat=,etime=,args="])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_else(|error| error.to_string());
        let dir = self.dir.path().to_string_lossy();
        listed
            .lines()
            .filter(|line| line.contains(dir.as_ref()) || line.contains(&self.session))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", self.report());
        }
        let _ = self.client.kill();
        let _ = self.client.wait();
        let _ = Self::zellij_in(self.dir.path())
            .args(["kill-all-sessions", "--yes"])
            .output();
    }
}

#[tokio::test]
async fn closing_a_pane_leaves_the_others_and_stale_instances_close_nothing() {
    let Some(mut server) = Isolated::start() else {
        return;
    };
    server.shown().await;
    let zellij = &server.zellij;
    zellij.ensure_supported().await.unwrap();
    let command = ["sleep".to_owned(), "600".into()];
    let opened = zellij
        .open_tab(&server.session, &server.shell(&command))
        .await
        .unwrap();
    assert_eq!(Some(opened.socket_dir.as_path()), zellij.socket_dir());
    let place = PaneRef {
        session: &opened.session,
        pane: &opened.pane,
    };
    let split = zellij
        .open_split(place, &server.shell(&command))
        .await
        .unwrap();
    let pane = open_pane(zellij, place, Some(&opened.instance)).await;
    assert_eq!(pane.tab_name, "placed");
    let panes = zellij.panes(&server.session).await.unwrap();
    assert_eq!(
        panes.len(),
        3,
        "the user's pane, the tab's and its split: {panes:?}; {}",
        what_zellij_lists(zellij, &server.session).await
    );

    zellij.close(place, Some("1:1")).await.unwrap();
    zellij.close(place, None).await.unwrap();
    open_pane(zellij, place, None).await;

    zellij.close(place, Some(&opened.instance)).await.unwrap();
    wait_until_gone(zellij, place).await;
    let split = PaneRef {
        session: &split.session,
        pane: &split.pane,
    };
    open_pane(zellij, split, None).await;
    let panes = zellij.panes(&server.session).await.unwrap();
    assert_eq!(
        panes.len(),
        2,
        "{panes:?}; {}",
        what_zellij_lists(zellij, &server.session).await
    );
    zellij.close(place, Some(&opened.instance)).await.unwrap();
}

#[tokio::test]
async fn typed_text_reaches_the_shell_as_it_is() {
    let Some(mut server) = Isolated::start() else {
        return;
    };
    server.shown().await;
    let zellij = &server.zellij;
    let opened = zellij
        .open_tab(&server.session, &server.shell(&[]))
        .await
        .unwrap();
    let place = PaneRef {
        session: &opened.session,
        pane: &opened.pane,
    };
    let instance = opened.instance.as_str();
    let marker = server.cwd().join("typed");
    let typed = |text: &str| format!("printf '%s;' '{text}' >> {};\r", marker.display());
    // Text for a pane under its ID on another server instance goes nowhere.
    assert!(zellij.write(place, "1:1", &typed("stale")).await.is_err());
    zellij
        .write(place, instance, &typed("-as typed"))
        .await
        .unwrap();
    wait_for_file(&marker, ";").await;
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "-as typed;");
    let screen = zellij.capture(place, instance, 50).await.unwrap();
    assert!(screen.contains("-as typed"), "{screen}");
    assert!(zellij.capture(place, "1:1", 50).await.is_err());
}

#[tokio::test]
async fn a_place_recorded_without_its_directory_reaches_its_server() {
    let Some(server) = Isolated::start() else {
        return;
    };
    let recorded = Zellij::recorded(&BTreeMap::new());
    assert_eq!(recorded.socket_dir(), None);
    let reached = recorded.reaching(&server.session).await.unwrap();
    let dir = reached.socket_dir().unwrap();
    assert!(
        same_dir(dir, server.dir.path().join("s").as_path()),
        "{dir:?}"
    );
    let users = PaneRef {
        session: &server.session,
        pane: "terminal_0",
    };
    open_pane(&reached, users, None).await;
}

/// A listing Zellij reports missing is asked again only while the server
/// runs, the recorded instance where there is one: a session restarted
/// meanwhile numbers its panes afresh.
#[tokio::test]
async fn a_listing_is_asked_again_only_on_the_recorded_instance() {
    let Some(server) = Isolated::start() else {
        return;
    };
    let session = server.session.as_str();
    let (instance, _) = server.zellij.instance(session).await.unwrap().unwrap();
    assert!(server.zellij.runs(session, Some(&instance)).await.unwrap());
    assert!(server.zellij.runs(session, None).await.unwrap());
    assert!(!server.zellij.runs(session, Some("1:0")).await.unwrap());
    assert!(!server.zellij.runs("ended", None).await.unwrap());
}

#[tokio::test]
async fn a_session_left_with_no_pane_ends() {
    let Some(mut server) = Isolated::start() else {
        return;
    };
    server.shown().await;
    let zellij = &server.zellij;
    let (instance, _) = zellij.instance(&server.session).await.unwrap().unwrap();
    let only = PaneRef {
        session: &server.session,
        pane: "terminal_0",
    };
    zellij.close(only, Some(&instance)).await.unwrap();
    for _ in 0..600 {
        if zellij.instance(&server.session).await.unwrap().is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{} never ended", server.session);
}

/// The pane `place`, which is open on `instance`, or any instance without
/// one; failing, what Zellij lists then tells why it was not found.
async fn open_pane(zellij: &Zellij, place: PaneRef<'_>, instance: Option<&str>) -> Pane {
    if let Some(pane) = zellij.pane(place, instance).await.unwrap() {
        return pane;
    }
    panic!(
        "{} is not open on {instance:?}; {}",
        place.pane,
        what_zellij_lists(zellij, place.session).await
    );
}

/// The server of `session` and what Zellij prints listing its panes, as
/// the reason for a failed assertion.
async fn what_zellij_lists(zellij: &Zellij, session: &str) -> String {
    let instance = zellij.instance(session).await;
    let mut list = zellij.action(session);
    list.args(["list-panes", "--all", "--json"]);
    let output = Run::new(list).timeout(TIMEOUT).capture().await;
    format!("the server is {instance:?} and Zellij lists {output:?}")
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
        .find_map(|dir| std::fs::read_to_string(dir.join(&file)).ok())
        .map_or_else(|| "none".to_owned(), |log| last_lines(&log, 60))
}

/// Every path under `dir`.
fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|entry| {
            let path = entry.path();
            let below = walkdir(&path);
            std::iter::once(path).chain(below)
        })
        .collect()
}

/// Wait until `place` is gone; the limit is only a hang guard.
async fn wait_until_gone(zellij: &Zellij, place: PaneRef<'_>) {
    for _ in 0..600 {
        if zellij.pane(place, None).await.unwrap().is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{} never closed", place.pane);
}

/// Wait until `path` holds text ending in `end`, as a shell in a pane writes
/// it; the limit is only a hang guard.
async fn wait_for_file(path: &Path, end: &str) {
    for _ in 0..600 {
        if std::fs::read_to_string(path).is_ok_and(|written| written.ends_with(end)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{} never ended in {end}", path.display());
}
