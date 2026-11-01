use super::*;

#[test]
fn the_tmux_variable_names_the_socket_and_the_session() {
    assert_eq!(
        parse_tmux_var("/tmp/tmux-501/default,4242,3"),
        Some(TmuxVar {
            socket: "/tmp/tmux-501/default",
            session: "$3".into(),
        })
    );
    assert_eq!(
        parse_tmux_var("/tmp/a,b/sock,1,0").map(|var| var.socket),
        Some("/tmp/a,b/sock")
    );
    assert_eq!(parse_tmux_var("/tmp/sock,1,x"), None);
    assert_eq!(parse_tmux_var(",1,2"), None);
    assert_eq!(parse_tmux_var("sock"), None);
}

#[test]
fn versions_read_major_and_minor() {
    assert_eq!(parse_version("tmux 3.5a\n"), Some((3, 5)));
    assert_eq!(parse_version("tmux 3.3"), Some((3, 3)));
    assert_eq!(parse_version("tmux next-3.6"), Some((3, 6)));
    assert_eq!(parse_version("tmux master"), None);
    assert!(parse_version("tmux 3.2a").is_some_and(|version| version < OLDEST));
    assert!(parse_version("tmux 10.0").is_some_and(|version| version >= OLDEST));
}

#[test]
fn session_names_drop_what_tmux_rejects() {
    assert_eq!(session_name("shoal.app: main"), "shoal_app_ main");
}

#[test]
fn panes_count_clients_only_on_the_current_window() {
    let shown = parse_pane("%3 @2 2 1 1 0 4321 99:1700000000 my window").unwrap();
    assert_eq!(
        shown,
        Pane {
            id: "%3".into(),
            window: "@2".into(),
            window_name: "my window".into(),
            window_panes: 2,
            clients: 1,
            in_mode: false,
            pid: 4321,
            instance: "99:1700000000".into(),
        }
    );
    let hidden = parse_pane("%3 @2 1 0 2 1 4321 99:1 w").unwrap();
    assert_eq!(hidden.clients, 0);
    assert!(hidden.in_mode);
    assert_eq!(parse_pane("%3 @2"), None);
}

#[test]
fn opened_panes_name_their_server() {
    assert_eq!(
        parse_opened("%7 99:1700000000 /tmp/tmux 501/default\n"),
        Some(Opened {
            pane: "%7".into(),
            instance: "99:1700000000".into(),
            socket: "/tmp/tmux 501/default".into(),
        })
    );
}

#[test]
fn windows_open_in_the_session_and_splits_beside_their_pane() {
    let command = ["/bin/sh".to_owned(), "-c".into(), "run".into()];
    let mut shell = Shell {
        session: "$2",
        label: "placed",
        cwd: Path::new("/work"),
        focus: false,
        command: &command,
        beside: None,
    };
    let words = |shell: &Shell<'_>| -> Vec<String> {
        open_args(shell)
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    };
    assert_eq!(
        words(&shell),
        [
            "new-window",
            "-t",
            "$2:",
            "-n",
            "placed",
            "-P",
            "-F",
            OPENED_FORMAT,
            "-c",
            "/work",
            "-d",
            "/bin/sh",
            "-c",
            "run",
        ]
    );
    shell.focus = true;
    shell.beside = Some("%4");
    assert_eq!(words(&shell)[..4], ["split-window", "-h", "-t", "%4"]);
    assert!(!words(&shell).contains(&"-d".to_owned()));
}

#[test]
fn pane_commands_run_only_on_the_instance_tmux_checks_as_it_runs_them() {
    let select = [
        words(&["select-window", "-t", "%4"]),
        words(&["select-pane", "-t", "%4"]),
    ];
    let discard = [words(&["delete-buffer", "-b", "b"])];
    assert_eq!(
        on_instance("%4", "99:1700000000", &select, &discard).unwrap(),
        [
            "if-shell",
            "-F",
            "-t",
            "%4",
            "#{==:#{pid}:#{start_time},99:1700000000}",
            "select-window -t %4 ; select-pane -t %4",
            "delete-buffer -b b",
        ]
    );
    assert!(on_instance("%4", "1,}", &select, &[]).is_err());
}

#[test]
fn arguments_ending_in_a_semicolon_stay_literal() {
    assert_eq!(literal("topic;"), "topic\\;");
    assert_eq!(literal("mid;dle"), "mid;dle");
    assert_eq!(literal(Path::new("/work")), "/work");
}

#[test]
fn a_missing_server_or_session_lists_no_panes() {
    assert!(nothing_to_list(
        "no server running on /tmp/tmux-501/default"
    ));
    assert!(nothing_to_list(
        "error connecting to /tmp/tmux-501/default (No such file or directory)"
    ));
    assert!(nothing_to_list("no current target"));
    assert!(nothing_to_list(
        "error connecting to /tmp/tmux-501/default (Connection refused)"
    ));
    assert!(!nothing_to_list(
        "error connecting to /tmp/tmux-501/default (Permission denied)"
    ));
    assert!(!nothing_to_list("can't find pane: %9"));
}

/// A tmux server of its own on a socket in a temporary directory, started
/// without the runner's tmux config, home or shell and killed on drop;
/// `None` without tmux on PATH, which these tests then skip.
struct Isolated {
    dir: tempfile::TempDir,
    tmux: Tmux,
}

impl Isolated {
    fn start() -> Option<Self> {
        let installed = std::process::Command::new(Tool::Tmux.program())
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success());
        if !installed {
            eprintln!("skipping: tmux is not installed");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("sock");
        // The server's environment is every pane's, and it outlives its
        // last session until the tests create one.
        let started = std::process::Command::new(Tool::Tmux.program())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", dir.path())
            .env("SHELL", "/bin/sh")
            .args(["-f", "/dev/null", "-S"])
            .arg(&socket)
            .args(["start-server", ";", "set-option", "-s", "exit-empty", "off"])
            .output()
            .unwrap();
        assert!(started.status.success(), "{started:?}");
        let tmux = Tmux {
            socket: Some(socket),
        };
        Some(Self { dir, tmux })
    }

    fn cwd(&self) -> &Path {
        self.dir.path()
    }

    fn shell<'a>(&'a self, session: &'a str, beside: Option<&'a str>) -> Shell<'a> {
        Shell {
            session,
            label: "placed",
            cwd: self.cwd(),
            focus: false,
            command: &[],
            beside,
        }
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        let mut kill = std::process::Command::new(Tool::Tmux.program());
        kill.arg("-S")
            .arg(self.dir.path().join("sock"))
            .arg("kill-server");
        let _ = kill.output();
    }
}

#[tokio::test]
async fn sessions_are_found_by_name_or_created() {
    let Some(server) = Isolated::start() else {
        return;
    };
    assert!(server.tmux.panes().await.unwrap().is_empty());
    let created = server
        .tmux
        .session("repo.name", server.cwd())
        .await
        .unwrap();
    assert!(created.session.starts_with('$'));
    assert_eq!(
        Some(Path::new(&created.socket)),
        server.tmux.socket.as_deref()
    );
    let found = server
        .tmux
        .session("repo.name", server.cwd())
        .await
        .unwrap();
    assert_eq!(found, created);
}

#[tokio::test]
async fn names_and_directories_ending_in_a_semicolon_reach_tmux_as_they_are() {
    let Some(server) = Isolated::start() else {
        return;
    };
    let tmux = &server.tmux;
    let cwd = server.cwd().join("dir;");
    std::fs::create_dir(&cwd).unwrap();
    let session = tmux.session("repo;", &cwd).await.unwrap().session;
    let mut shell = server.shell(&session, None);
    shell.label = "topic;";
    shell.cwd = &cwd;
    let opened = tmux.open(&shell).await.unwrap();
    let pane = tmux
        .pane(&opened.pane, Some(&opened.instance))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pane.window_name, "topic;");
    let listed = tmux
        .run(&["list-sessions", "-F", "#{session_name}"])
        .await
        .unwrap();
    assert_eq!(listed.trim(), "repo;");
    let path = tmux
        .run(&[
            "display-message",
            "-p",
            "-t",
            &opened.pane,
            "#{pane_current_path}",
        ])
        .await
        .unwrap();
    assert!(path.trim_end().ends_with("dir;"), "{path}");
}

#[tokio::test]
async fn a_server_tmux_cannot_reach_keeps_its_panes_from_cleanup() {
    use std::os::unix::fs::PermissionsExt as _;
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: root reaches a socket whatever its mode");
        return;
    }
    let Some(server) = Isolated::start() else {
        return;
    };
    let tmux = &server.tmux;
    let session = tmux.session("repo", server.cwd()).await.unwrap().session;
    let opened = tmux.open(&server.shell(&session, None)).await.unwrap();
    let socket = tmux.socket.clone().unwrap();
    let mode = |bits| std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(bits));
    mode(0o000).unwrap();
    let listed = tmux.panes().await;
    let closed = tmux.close(&opened.pane, Some(&opened.instance)).await;
    mode(0o700).unwrap();
    assert!(listed.is_err(), "{listed:?}");
    assert!(closed.is_err(), "a refused socket proves no pane gone");
    assert!(tmux.pane(&opened.pane, None).await.unwrap().is_some());
}

#[tokio::test]
async fn closing_a_pane_leaves_the_others() {
    let Some(server) = Isolated::start() else {
        return;
    };
    let tmux = &server.tmux;
    let session = tmux.session("repo", server.cwd()).await.unwrap().session;
    let opened = tmux.open(&server.shell(&session, None)).await.unwrap();
    assert_eq!(Some(Path::new(&opened.socket)), tmux.socket.as_deref());
    let split = tmux
        .open(&server.shell(&session, Some(&opened.pane)))
        .await
        .unwrap();
    let pane = tmux
        .pane(&opened.pane, Some(&opened.instance))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pane.window_name, "placed");
    assert_eq!(pane.window_panes, 2);

    tmux.close(&opened.pane, Some("1:1")).await.unwrap();
    tmux.close(&opened.pane, None).await.unwrap();
    assert!(tmux.pane(&opened.pane, None).await.unwrap().is_some());

    tmux.close(&opened.pane, Some(&opened.instance))
        .await
        .unwrap();
    assert!(tmux.pane(&opened.pane, None).await.unwrap().is_none());
    assert!(tmux.pane(&split.pane, None).await.unwrap().is_some());
    assert_eq!(tmux.panes().await.unwrap().len(), 2);
    tmux.close(&opened.pane, Some(&opened.instance))
        .await
        .unwrap();
}

#[tokio::test]
async fn typed_text_reaches_the_shell_as_it_is() {
    let Some(server) = Isolated::start() else {
        return;
    };
    let tmux = &server.tmux;
    let session = tmux.session("repo", server.cwd()).await.unwrap().session;
    let opened = tmux.open(&server.shell(&session, None)).await.unwrap();
    let instance = opened.instance.as_str();
    let marker = server.cwd().join("typed");
    let typed = |text: &str| format!("printf '%s;' '{text}' >> {};\r", marker.display());
    // Text for a pane under its ID on another server instance goes nowhere.
    tmux.send(&opened.pane, "1:1", &typed("stale"))
        .await
        .unwrap();
    tmux.send(&opened.pane, instance, &typed("as typed"))
        .await
        .unwrap();
    wait_for_file(&marker, ";").await;
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "as typed;");
    let buffers = tmux.run(&["list-buffers"]).await.unwrap();
    assert_eq!(buffers, "", "a discarded paste leaves its buffer");
    let screen = tmux.capture(&opened.pane, instance, 50).await.unwrap();
    assert!(screen.contains("as typed"), "{screen}");
    // A pane under its ID on another server instance shows nothing.
    let stale = tmux.capture(&opened.pane, "1:1", 50).await.unwrap();
    assert_eq!(stale, "");
}

#[tokio::test]
async fn a_pane_runs_its_command_until_respawned_on_its_instance() {
    let Some(server) = Isolated::start() else {
        return;
    };
    let tmux = &server.tmux;
    let session = tmux.session("repo", server.cwd()).await.unwrap().session;
    let ran = server.cwd().join("ran");
    let command = [
        "/bin/sh".to_owned(),
        "-c".into(),
        format!("printf 'ran;' > {}; exec sleep 600", ran.display()),
    ];
    let mut shell = server.shell(&session, None);
    shell.command = &command;
    let opened = tmux.open(&shell).await.unwrap();
    wait_for_file(&ran, ";").await;
    assert_eq!(std::fs::read_to_string(&ran).unwrap(), "ran;");

    // A pane under its ID on another server instance is left alone, and
    // the command reaches the pane through tmux's quoting as it is.
    let writes = |path: &Path| {
        let script = format!("printf 'again;' > \"{}\"; exec sleep 600", path.display());
        ["/bin/sh".to_owned(), "-c".into(), script]
    };
    let stale = server.cwd().join("stale");
    tmux.respawn(&opened.pane, "1:1", &writes(&stale))
        .await
        .unwrap();
    let respawned = server.cwd().join("respawned");
    tmux.respawn(&opened.pane, &opened.instance, &writes(&respawned))
        .await
        .unwrap();
    wait_for_file(&respawned, ";").await;
    assert!(!stale.exists());
    assert!(
        tmux.pane(&opened.pane, Some(&opened.instance))
            .await
            .unwrap()
            .is_some()
    );
}

/// Wait until `path` holds text ending in `end`, as a shell in a pane writes
/// it; the limit is only a hang guard.
async fn wait_for_file(path: &Path, end: &str) {
    for _ in 0..600 {
        if std::fs::read_to_string(path).is_ok_and(|written| written.ends_with(end)) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{} never ended in {end}", path.display());
}
