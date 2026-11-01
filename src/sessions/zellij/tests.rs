use std::{
    os::{fd::FromRawFd as _, unix::process::CommandExt as _},
    process::Stdio,
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
        {"id": 0, "is_plugin": true, "tab_id": 0, "tab_name": "Tab #1", "exited": false},
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
}

/// A Zellij session of its own, in a socket directory short enough for its
/// socket path, started without the runner's Zellij config, home or shell
/// and ended on drop, with a client on a terminal of its own showing it,
/// since Zellij drops tabs opened in a session no client shows; `None`
/// without Zellij on PATH, which these tests then skip.
struct Isolated {
    dir: tempfile::TempDir,
    zellij: Zellij,
    session: &'static str,
    client: std::process::Child,
}

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
        let session = "repo";
        let started = Self::zellij_in(dir.path())
            .args(["attach", "--create-background", session])
            .output()
            .unwrap();
        assert!(started.status.success(), "{started:?}");
        let client = Self::client(dir.path(), session);
        let zellij = Zellij {
            socket_dir: Some(dir.path().join("s")),
        };
        Some(Self {
            dir,
            zellij,
            session,
            client,
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
    /// output a thread drains.
    fn client(dir: &Path, session: &str) -> std::process::Child {
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
                &mut size,
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
            let _ = std::io::copy(&mut master, &mut std::io::sink());
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
    async fn shown(&self) {
        for _ in 0..600 {
            if !self.zellij.clients(self.session).await.unwrap().is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("no client attached to {}", self.session);
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        let _ = self.client.kill();
        let _ = self.client.wait();
        let _ = Self::zellij_in(self.dir.path())
            .args(["kill-all-sessions", "--yes"])
            .output();
    }
}

#[tokio::test]
async fn closing_a_pane_leaves_the_others_and_stale_instances_close_nothing() {
    let Some(server) = Isolated::start() else {
        return;
    };
    server.shown().await;
    let zellij = &server.zellij;
    zellij.ensure_supported().await.unwrap();
    let command = ["sleep".to_owned(), "600".into()];
    let opened = zellij
        .open_tab(server.session, &server.shell(&command))
        .await
        .unwrap();
    assert!(!opened.own_session);
    assert_eq!(Some(opened.socket_dir.as_path()), zellij.socket_dir());
    let place = PaneRef {
        session: &opened.session,
        pane: &opened.pane,
    };
    let split = zellij
        .open_split(place, &server.shell(&command))
        .await
        .unwrap();
    let pane = zellij
        .pane(place, Some(&opened.instance))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pane.tab_name, "placed");
    assert_eq!(
        zellij.panes(server.session).await.unwrap().len(),
        3,
        "the user's pane, the tab's and its split"
    );

    zellij.close(place, Some("1:1"), false).await.unwrap();
    zellij.close(place, None, false).await.unwrap();
    assert!(zellij.pane(place, None).await.unwrap().is_some());

    zellij
        .close(place, Some(&opened.instance), false)
        .await
        .unwrap();
    wait_until_gone(zellij, place).await;
    let split = PaneRef {
        session: &split.session,
        pane: &split.pane,
    };
    assert!(zellij.pane(split, None).await.unwrap().is_some());
    assert_eq!(zellij.panes(server.session).await.unwrap().len(), 2);
    zellij
        .close(place, Some(&opened.instance), false)
        .await
        .unwrap();
}

#[tokio::test]
async fn typed_text_reaches_the_shell_as_it_is() {
    let Some(server) = Isolated::start() else {
        return;
    };
    server.shown().await;
    let zellij = &server.zellij;
    let opened = zellij
        .open_tab(server.session, &server.shell(&[]))
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
