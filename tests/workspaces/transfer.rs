//! Moving workspaces: export, import, `move ssh` and `migrate`, and the agent
//! sessions that go with them.
use super::herdr::install_fake_herdr;
use super::zmx::wait_zmx_sessions_ended;
use super::*;

#[test]
fn export_and_import_move_a_workspace_through_a_file_or_a_pipe() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './agent.sh']\nresume_command = ['sh', './resume.sh']\n",
    ));
    let workspace = fixture.add("moving");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(path.join("notes"), "draft\n").unwrap();
    // When the agent starts again after the move, it records its arguments
    // and runs until the test releases it, or for at most two minutes.
    fs::write(
        path.join("agent.sh"),
        "if [ -e agent-started ]; then printf '%s\\n' \"$@\" > agent-args; \
         i=0; while [ ! -e release ] && [ $i -lt 120 ]; do sleep 1; i=$((i + 1)); done; \
         exit 0; fi\n\
         trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(path.join("resume.sh"), "touch resumed\n").unwrap();
    let agent = fixture
        .command()
        .args(["claude", "moving"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("agent started", || path.join("agent-started").exists());

    let inside = path.join("moving.shoal");
    let refused = fixture.run(&["move", "export", "moving", "-o", inside.to_str().unwrap()]);
    assert!(!refused.status.success());
    assert!(path.exists() && !inside.exists());
    let file = fixture.root.path().join("moving.shoal");
    let file = file.to_str().unwrap();
    let exported = fixture.ok(&["move", "export", "moving", "-o", file]);
    assert!(agent.wait_with_output().unwrap().status.success());
    assert_eq!(exported["manifest"]["workspace"]["name"], "moving");
    assert!(exported["manifest"]["bundle"].as_bool().unwrap());
    let agents = exported["manifest"]["workspace"]["agents"]
        .as_array()
        .unwrap();
    assert_eq!(agents.len(), 1, "{exported}");
    assert_eq!(agents[0]["record"]["moved"], true);
    assert!(!path.exists());
    // The repository has no origin to select the importing one by.
    let unselected = fixture.run(&["move", "import", file]);
    assert!(!unselected.status.success());

    let repo = fixture.repo.to_str().unwrap();
    let imported = fixture.ok(&["move", "import", file, "--repo", repo]);
    assert_eq!(imported["workspace"]["name"], "moving");
    assert_eq!(imported["workspace"]["state"], "ready");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
    assert_eq!(fixture.ok(&["restore"]), serde_json::json!([]));
    // The agent never reported a session, so it starts as a new agent
    // does, told the workspace moved.
    let record = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(imported["workspace"]["id"].as_str().unwrap())
        .join(format!(
            "{}.recovery.json",
            agents[0]["execution"].as_str().unwrap()
        ));
    assert!(record.exists());
    let resumed = fixture
        .command()
        .args(["resume", "moving", "--focus"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("agent started again", || path.join("agent-args").exists());
    // Its wrapper consumes the moved record once the agent registers, while
    // the agent still runs.
    wait_until("moved record consumed", || !record.exists());
    fs::write(path.join("release"), "").unwrap();
    let resumed = resumed.wait_with_output().unwrap();
    assert!(resumed.status.success(), "{resumed:?}");
    assert!(!path.join("resumed").exists());

    // Without a file, a piped export writes to standard output and a piped
    // import reads standard input. An origin that is a local path selects
    // the repository, as a URL does.
    let origin = fixture.root.path().join("origin.git");
    git(
        fixture.root.path(),
        &["init", "--quiet", "--bare", "origin.git"],
    );
    git(
        &fixture.repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    let exported = fixture
        .command()
        .args(["move", "export", "moving"])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let mut import = fixture
        .command()
        .args(["--json", "move", "import"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    import
        .stdin
        .take()
        .unwrap()
        .write_all(&exported.stdout)
        .unwrap();
    let imported = import.wait_with_output().unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let imported: Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(imported["workspace"]["name"], "moving");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
}

#[test]
fn import_resume_queues_the_agents_the_export_stopped_once_post_setup_succeeds() {
    let fixture = Fixture::with_config(Some(&format!(
        "{UNLIMITED_AGENTS}[agents.claude]\ncommand = ['sh', './agent.sh']\n"
    )));
    // Once the test arms it, the post-setup hook fails; otherwise it marks
    // the workspace set up.
    let armed = fixture.root.path().join("fail-post-setup");
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        format!(
            "#!/bin/sh\nif [ -e '{}' ]; then exit 3; fi\ntouch post-setup-done\n",
            armed.display()
        ),
    );
    commit_resource_config(&fixture.repo, "hooks.post_setup = 'post-setup.sh'\n");
    // Queued restores start in detached zmx sessions.
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let workspace = fixture.add("handed");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::remove_file(path.join("post-setup-done")).unwrap();
    // The agent runs until it is stopped; started again, it records whether
    // post-setup ran first and runs until the test releases it, or for at
    // most two minutes.
    fs::write(
        path.join("agent.sh"),
        "if [ -e agent-started ]; then \
         if [ -e post-setup-done ]; then touch agent-restarted; else touch agent-before-setup; fi; \
         i=0; while [ ! -e release ] && [ $i -lt 120 ]; do sleep 1; i=$((i + 1)); done; \
         exit 0; fi\n\
         trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    let agent = fixture
        .command()
        .args(["claude", "handed"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("agent started", || path.join("agent-started").exists());
    wait_registered_execution(&fixture, "handed");
    let repo = fixture.repo.to_str().unwrap();
    let export = |name: &str| {
        let file = fixture.root.path().join(name);
        fixture.ok(&["move", "export", "handed", "-o", file.to_str().unwrap()]);
        file
    };
    let file = export("first.shoal");
    assert!(agent.wait_with_output().unwrap().status.success());

    // A failed post-setup leaves the agents stopped.
    fs::write(&armed, "").unwrap();
    let failed = fixture.run(&[
        "move",
        "import",
        file.to_str().unwrap(),
        "--repo",
        repo,
        "--resume",
    ]);
    assert!(!failed.status.success(), "{failed:?}");
    assert_eq!(fixture.ok(&["queue"]), serde_json::json!([]));
    assert!(path.exists() && !path.join("post-setup-done").exists());

    fs::remove_file(&armed).unwrap();
    let file = export("second.shoal");
    let imported = fixture.ok(&[
        "move",
        "import",
        file.to_str().unwrap(),
        "--repo",
        repo,
        "--resume",
    ]);
    let queued = imported["queued"]["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1, "{imported}");
    wait_until("agent restarted", || {
        path.join("agent-restarted").exists() || path.join("agent-before-setup").exists()
    });
    fs::write(path.join("release"), "").unwrap();
    wait_zmx_sessions_ended(fixture.root.path());
    assert!(path.join("agent-restarted").exists());
}

/// Install an `ssh` in `fixture`'s PATH that runs the remote command in
/// `other`'s home and state, as on another machine, and refuses the host
/// `down`.
fn install_ssh_to(fixture: &Fixture, other: &Fixture) {
    let other = other.root.path().display();
    install_test_script(
        fixture.root.path().join("bin/ssh"),
        format!(
            "#!/bin/sh\n\
             [ \"$1\" = -- ] && shift\n\
             if [ \"$1\" = down ]; then echo 'ssh: connect to host down: refused' >&2; exit 255; fi\n\
             shift\n\
             cd '{other}' || exit 255\n\
             export HOME='{other}' ZDOTDIR='{other}' SHOAL_STATE_DIR='{other}/state' ZMX_DIR='{other}/zmx'\n\
             export PATH='{other}/bin':\"$PATH\"\n\
             exec sh -c \"$*\"\n"
        ),
    );
}

/// Two machines with `config`, whose repositories are clones of one origin,
/// named by a URL as remotes usually are, which import matches, and where
/// `ssh mini` on the first reaches the second.
fn two_machines(config: Option<&str>) -> (Fixture, Fixture) {
    let here = Fixture::with_config(config);
    let there = Fixture::with_config(config);
    git(
        here.root.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    let origin = fs::canonicalize(here.root.path().join("origin.git")).unwrap();
    let origin = format!("file://{}", origin.display());
    git(&here.repo, &["remote", "add", "origin", &origin]);
    git(&here.repo, &["push", "-q", "-u", "origin", "HEAD:main"]);
    git(&here.repo, &["remote", "set-head", "origin", "main"]);
    git(there.root.path(), &["clone", "-q", &origin, "clone"]);
    there.ok(&[
        "repo",
        "add",
        there.root.path().join("clone").to_str().unwrap(),
    ]);
    install_ssh_to(&here, &there);
    (here, there)
}

#[test]
fn move_ssh_hands_a_workspace_and_its_agent_to_another_shoal() {
    let config = format!("{UNLIMITED_AGENTS}[agents.claude]\ncommand = ['sh', './agent.sh']\n");
    let (here, there) = two_machines(Some(&config));
    // Queued restores start in detached zmx sessions.
    install_test_script(there.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);

    let workspace = here.add("handed");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    // The agent runs until it is stopped; started again, it records that and
    // runs until the test releases it, or for at most two minutes.
    fs::write(
        path.join("agent.sh"),
        "if [ -e agent-started ]; then touch agent-restarted; \
         i=0; while [ ! -e release ] && [ $i -lt 120 ]; do sleep 1; i=$((i + 1)); done; \
         exit 0; fi\n\
         trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    let agent = here
        .command()
        .args(["claude", "handed"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("agent started", || path.join("agent-started").exists());
    wait_registered_execution(&here, "handed");

    // A machine ssh cannot reach removes nothing.
    let shoal = support::shoal_binary();
    let refused = here.run(&["move", "ssh", "down", "handed", "--shoal", shoal]);
    assert!(!refused.status.success());
    assert!(path.join("agent.sh").exists());
    // Neither does a Shoal ssh's PATH cannot find there.
    let missing = here.run(&["move", "ssh", "mini", "handed", "--shoal", "missing-shoal"]);
    assert!(!missing.status.success());
    assert!(path.join("agent.sh").exists());

    let moved = here.ok(&["move", "ssh", "mini", "handed", "--shoal", shoal]);
    assert!(agent.wait_with_output().unwrap().status.success());
    assert_eq!(moved["failed"], serde_json::json!([]), "{moved}");
    let imported = &moved["moved"][0];
    assert_eq!(imported["workspace"]["name"], "handed", "{moved}");
    assert_eq!(imported["queued"]["queued"].as_array().unwrap().len(), 1);
    assert!(!path.exists());
    let moved_path = Path::new(imported["workspace"]["path"].as_str().unwrap()).to_owned();
    assert!(moved_path.starts_with(fs::canonicalize(there.root.path()).unwrap()));
    wait_until("agent restarted there", || {
        moved_path.join("agent-restarted").exists()
    });
    fs::write(moved_path.join("release"), "").unwrap();
    wait_zmx_sessions_ended(there.root.path());
}

#[test]
fn move_ssh_keep_copies_workspaces_there_and_leaves_them_here() {
    let (here, there) = two_machines(None);
    let paths: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| {
            let workspace = here.add(name);
            let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
            fs::write(path.join("notes"), format!("{name}\n")).unwrap();
            path
        })
        .collect();
    let shoal = support::shoal_binary();
    let ssh = |extra: &[&'static str]| {
        let mut args = vec!["move", "ssh", "mini", "first", "second", "--keep"];
        args.extend(extra);
        args.extend(["--shoal", shoal]);
        args
    };

    // A checkpoint number names one workspace's checkpoint.
    let refused = here.run(&ssh(&["--checkpoint", "1"]));
    assert!(!refused.status.success());
    assert_eq!(there.ok(&["list"]), serde_json::json!([]));

    let copied = here.ok(&ssh(&[]));
    assert_eq!(copied["failed"], serde_json::json!([]), "{copied}");
    let copies = copied["copied"].as_array().unwrap();
    assert_eq!(copies.len(), 2, "{copied}");
    let there_root = fs::canonicalize(there.root.path()).unwrap();
    for ((copy, path), name) in copies.iter().zip(&paths).zip(["first", "second"]) {
        assert_eq!(
            fs::read_to_string(path.join("notes")).unwrap(),
            format!("{name}\n")
        );
        let copy_path = Path::new(copy["workspace"]["path"].as_str().unwrap());
        assert!(copy_path.starts_with(&there_root), "{copied}");
        assert_eq!(
            fs::read_to_string(copy_path.join("notes")).unwrap(),
            format!("{name}\n")
        );
    }
    assert_eq!(here.ok(&["list"]).as_array().unwrap().len(), 2);
}

#[test]
fn export_all_checks_every_workspace_first_and_import_takes_the_directory() {
    let fixture = Fixture::new();
    // Once the test arms it, the post-setup hook fails in the second workspace.
    let armed = fixture.root.path().join("fail-post-setup");
    install_test_script(
        fixture.repo.join("post-setup.sh"),
        format!(
            "#!/bin/sh\nif [ -e '{}' ]; then case \"$PWD\" in */second) exit 3;; esac; fi\n",
            armed.display()
        ),
    );
    commit_resource_config(&fixture.repo, "hooks.post_setup = 'post-setup.sh'\n");
    let paths: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| {
            let workspace = fixture.add(name);
            let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
            fs::write(path.join("notes"), format!("{name}\n")).unwrap();
            path
        })
        .collect();
    fixture.ok(&[
        "acquire",
        "hold",
        "--workspace",
        "second",
        "--name",
        "review",
    ]);
    let repo = fixture.repo.to_str().unwrap();
    let directory = fixture.root.path().join("exports");
    let export = |extra: &[&str]| {
        let mut args = vec!["move", "export", "--repo", repo, "--all", "-o"];
        args.push(directory.to_str().unwrap());
        args.extend(extra);
        fixture.run(&args)
    };

    // The hold on one workspace keeps both.
    let refused = export(&[]);
    assert!(!refused.status.success());
    assert!(paths.iter().all(|path| path.exists()));
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    // So does a file that one of them would overwrite.
    fs::write(directory.join("first.shoal"), "").unwrap();
    let refused = export(&["--yes"]);
    assert!(!refused.status.success());
    assert!(paths.iter().all(|path| path.exists()));
    fs::remove_file(directory.join("first.shoal")).unwrap();
    // So does one still in a lifecycle operation, which removal refuses.
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    let set_state = |state: &str| {
        db.execute(
            "UPDATE workspaces SET state=?1 WHERE name='second'",
            [state],
        )
        .unwrap();
    };
    set_state("updating");
    let refused = export(&["--yes"]);
    assert!(!refused.status.success());
    assert!(paths.iter().all(|path| path.exists()));
    set_state("ready");

    let exported = export(&["--yes", "--json"]);
    assert!(exported.status.success(), "{exported:?}");
    let exported: Value = serde_json::from_slice(&exported.stdout).unwrap();
    let names: Vec<_> = exported["exported"]
        .as_array()
        .unwrap()
        .iter()
        .map(|export| export["manifest"]["workspace"]["name"].clone())
        .collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(exported["failed"], serde_json::json!([]));
    assert!(paths.iter().all(|path| !path.exists()));
    assert!(directory.join("first.shoal").exists() && directory.join("second.shoal").exists());

    // A file that is no export fails alone; the others import, and a failed
    // post-setup hook fails its file without removing the workspace.
    fs::write(directory.join("broken.shoal"), "not an export\n").unwrap();
    fs::write(&armed, "").unwrap();
    let imported = fixture
        .command()
        .args([
            "--json",
            "move",
            "import",
            directory.to_str().unwrap(),
            "--repo",
            repo,
        ])
        .output()
        .unwrap();
    assert_eq!(imported.status.code(), Some(1), "{imported:?}");
    let imported: Value = serde_json::from_slice(&imported.stdout).unwrap();
    let names: Vec<_> = imported["imported"]
        .as_array()
        .unwrap()
        .iter()
        .map(|import| import["workspace"]["name"].clone())
        .collect();
    assert_eq!(names, ["first", "second"]);
    let failed: Vec<_> = imported["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| {
            Path::new(failure["file"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(failed, ["broken.shoal", "second.shoal"], "{imported}");
    for (path, name) in paths.iter().zip(["first", "second"]) {
        assert_eq!(
            fs::read_to_string(path.join("notes")).unwrap(),
            format!("{name}\n")
        );
    }
}

#[test]
fn export_all_keep_copies_every_space_past_one_that_cannot_be_copied() {
    let fixture = Fixture::new();
    let paths: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| Path::new(fixture.add(name)["path"].as_str().unwrap()).to_owned())
        .collect();
    let repo = fixture.repo.to_str().unwrap();
    let directory = fixture.root.path().join("copies");
    fs::create_dir(&directory).unwrap();
    // The file the first copy would overwrite fails that copy alone.
    fs::write(directory.join("first.shoal"), "").unwrap();
    let copied = fixture
        .command()
        .args([
            "--json", "move", "export", "--repo", repo, "--all", "--keep",
        ])
        .args(["-o", directory.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(copied.status.code(), Some(1), "{copied:?}");
    let copied: Value = serde_json::from_slice(&copied.stdout).unwrap();
    let names: Vec<_> = copied["exported"]
        .as_array()
        .unwrap()
        .iter()
        .map(|export| export["manifest"]["workspace"]["name"].clone())
        .collect();
    assert_eq!(names, ["second"], "{copied}");
    assert_eq!(copied["failed"][0]["workspace"], "first", "{copied}");
    assert!(paths.iter().all(|path| path.exists()));
    assert!(fs::metadata(directory.join("second.shoal")).unwrap().len() > 0);
}

#[test]
fn export_of_several_workspaces_writes_a_file_each_to_the_directory() {
    let fixture = Fixture::new();
    let paths: Vec<_> = ["first", "second", "third"]
        .into_iter()
        .map(|name| PathBuf::from(fixture.add(name)["path"].as_str().unwrap()))
        .collect();
    let directory = fixture.root.path().join("exports");
    let refused = fixture.run(&["move", "export", "first", "second", "-o", "-"]);
    assert!(!refused.status.success());
    assert!(paths.iter().all(|path| path.exists()));
    let output = directory.to_str().unwrap();
    let exported = fixture.ok(&["move", "export", "first", "second", "-o", output, "--yes"]);
    assert_eq!(
        exported["exported"].as_array().unwrap().len(),
        2,
        "{exported}"
    );
    assert_eq!(exported["failed"], serde_json::json!([]));
    for name in ["first", "second"] {
        assert!(directory.join(format!("{name}.shoal")).exists());
    }
    assert!(!paths[0].exists() && !paths[1].exists() && paths[2].exists());
    // An existing file is refused before the picker opens.
    let existing = fixture.root.path().join("existing.shoal");
    fs::write(&existing, "").unwrap();
    let (refused, rows) = fixture.pick(
        &["move", "export", "-o", existing.to_str().unwrap()],
        "",
        "",
    );
    assert!(!refused.status.success(), "{refused:?}");
    assert!(rows.is_empty(), "{rows}");
    // An existing directory takes the workspaces the picker selects.
    let third = fixture.ok(&["status", "third"])["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (picked, _) = fixture.pick(&["move", "export", "-o", output, "--yes"], &third, "");
    assert!(picked.status.success(), "{picked:?}");
    assert!(directory.join("third.shoal").exists());
    assert!(!paths[2].exists());
}

// A reader that goes away still lets the temporary archive be removed.
#[test]
fn streaming_export_to_a_closed_reader_removes_its_temporary_archive() {
    let fixture = Fixture::new();
    fixture.add("streamed");
    let temporary = fixture.root.path().join("cli-tmp");
    fs::create_dir(&temporary).unwrap();
    let exported = support::close_stdout(&mut fixture.command())
        .args(["move", "export", "streamed", "-o", "-"])
        .env("TMPDIR", &temporary)
        .output()
        .unwrap();
    assert_eq!(exported.status.code(), Some(141), "{exported:?}");
    assert!(exported.stderr.is_empty(), "{exported:?}");
    assert_eq!(fs::read_dir(&temporary).unwrap().count(), 0);
}

#[test]
fn streaming_export_refuses_a_herdr_tab_that_closes_with_the_workspace() {
    let fixture = Fixture::new();
    install_fake_herdr(&fixture);
    let workspace = fixture.add("tabbed");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    // The pane's watcher records that its tab closes with the workspace.
    let state = fixture
        .root
        .path()
        .join("state/workspaces")
        .join(workspace["id"].as_str().unwrap());
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("closing-herdr-w1:p7"), "").unwrap();
    let export = |pane: &str| {
        fixture
            .command()
            .args(["move", "export", "tabbed", "-o", "-"])
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", pane)
            .output()
            .unwrap()
    };
    let refuses = |pane: &str| {
        let refused = export(pane);
        assert!(!refused.status.success());
        assert!(refused.stdout.is_empty());
        assert!(path.exists());
    };
    // The watched pane, and another pane of its tab, which closes with it.
    refuses("w1:p7");
    refuses("w1:p7.2");
    // Watchers of earlier releases mark the tab itself.
    fs::remove_file(state.join("closing-herdr-w1:p7")).unwrap();
    for marker in ["closing-herdr-w1:t7", "closing-tab-w1:t7"] {
        fs::write(state.join(marker), "").unwrap();
        refuses("w1:p7");
        fs::remove_file(state.join(marker)).unwrap();
    }
    fs::write(state.join("closing-herdr-w1:p7"), "").unwrap();
    // A pane of another tab keeps its stream.
    let exported = export("w1:p8");
    assert!(exported.status.success(), "{exported:?}");
    assert!(exported.stdout.starts_with(b"shoal workspace 1\n"));
    assert!(!path.exists());
}

#[test]
fn export_finishes_when_its_terminal_ends_after_the_removal() {
    // The removal hook holds the daemon between removal and the export, as
    // long as closing a workspace's Herdr tab would take.
    let fixture = Fixture::with_config(Some("hooks.post_remove = 'hold.sh'\n"));
    install_test_script(
        fixture.repo.join("hold.sh"),
        "#!/bin/sh\ntouch removed\ni=0\nwhile [ ! -e go ] && [ $i -lt 50 ]; do sleep 1; i=$((i + 1)); done\n",
    );
    let workspace = fixture.add("cut");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(path.join("notes"), "draft\n").unwrap();
    let file = fixture.root.path().join("cut.shoal");
    let mut export = fixture
        .command()
        .args(["move", "export", "cut", "-o", file.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("workspace removed", || {
        fixture.repo.join("removed").exists()
    });
    export.kill().unwrap();
    export.wait().unwrap();
    fs::write(fixture.repo.join("go"), "").unwrap();

    wait_until("export written", || file.exists());
    let repo = fixture.repo.to_str().unwrap();
    let imported = fixture.ok(&["move", "import", file.to_str().unwrap(), "--repo", repo]);
    assert_eq!(imported["workspace"]["name"], "cut");
    assert_eq!(fs::read_to_string(path.join("notes")).unwrap(), "draft\n");
}

#[test]
fn import_on_the_exporting_machine_continues_the_session_unless_told_not_to() {
    let fixture = Fixture::with_config(Some("[agents.claude]\ncommand = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("carried");
    let path = Path::new(workspace["path"].as_str().unwrap());
    // The agent reports its session as Claude Code's hooks do, also when it
    // resumes one, then quits.
    fs::write(
        path.join("agent.sh"),
        format!(
            "session=carried-session\n\
             if [ \"$1\" = --resume ]; then printf '%s' \"$2\" > resumed; session=$2; fi\n\
             printf '{{\"session_id\":\"%s\"}}' \"$session\" | '{}' internal agent-state idle >/dev/null || exit 3\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["claude", "carried"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let file = fixture.root.path().join("carried.shoal");
    let file = file.to_str().unwrap();
    let repo = fixture.repo.to_str().unwrap();
    let resume = || {
        let output = fixture
            .command()
            .current_dir(path)
            .args(["resume", "--focus"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };

    let exported = fixture.ok(&["move", "export", "carried", "-o", file]);
    // The import trusts the workspace's path for the agent it resumes there.
    let config = fixture.root.path().join(".claude.json");
    fs::remove_file(&config).unwrap();
    let imported = fixture.ok(&["move", "import", file, "--repo", repo]);
    assert_eq!(imported["warnings"], serde_json::json!([]));
    // A machine without an ID cannot tell it exported the workspace.
    if exported["manifest"]["sessions"]["machine"].is_null() {
        resume();
        assert!(!path.join("resumed").exists());
        return;
    }
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(
        root["projects"][checkout.to_str().unwrap()]["hasTrustDialogAccepted"],
        true
    );
    resume();
    assert_eq!(
        fs::read_to_string(path.join("resumed")).unwrap(),
        "carried-session"
    );

    // --new-sessions starts the agent over on this machine too.
    fs::remove_file(path.join("resumed")).unwrap();
    fs::remove_file(file).unwrap();
    let exported = fixture.ok(&["move", "export", "carried", "-o", file]);
    assert_eq!(
        exported["manifest"]["sessions"]["last_sessions"][0]["session"], "carried-session",
        "{exported}"
    );
    fixture.ok(&["move", "import", file, "--repo", repo, "--new-sessions"]);
    resume();
    assert!(!path.join("resumed").exists());
}

#[test]
fn export_carries_session_files_that_import_puts_back() {
    let fixture = Fixture::with_config(Some("[agents.claude]\ncommand = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("transcript");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join("agent.sh"),
        format!(
            "session=moved-session\n\
             if [ \"$1\" = --resume ]; then printf '%s' \"$2\" > resumed; session=$2; fi\n\
             printf '{{\"session_id\":\"%s\"}}' \"$session\" | '{}' internal agent-state idle >/dev/null || exit 3\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["claude", "transcript"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // Claude Code keeps the transcript in its project's directory, beside a
    // directory of the session's subagents, and another session's files.
    let projects = fixture.root.path().join(".claude/projects");
    let project = projects.join("-elsewhere");
    fs::create_dir_all(project.join("moved-session/subagents")).unwrap();
    fs::write(project.join("moved-session.jsonl"), "{\"turn\":1}\n").unwrap();
    fs::write(project.join("moved-session/subagents/a.jsonl"), "sub\n").unwrap();
    fs::write(project.join("other-session.jsonl"), "other\n").unwrap();

    let file = fixture.root.path().join("transcript.shoal");
    let file = file.to_str().unwrap();
    let exported = fixture.ok(&["move", "export", "transcript", "-o", file]);
    let carried: Vec<_> = exported["manifest"]["session_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        carried,
        [
            "-elsewhere/moved-session.jsonl",
            "-elsewhere/moved-session/subagents/a.jsonl"
        ],
        "{exported}"
    );

    // The importing machine has none of them; the import puts them back, and
    // the agent continues its session.
    fs::remove_dir_all(&projects).unwrap();
    // The import records the workspace but cannot open it while another
    // branch has its name; its sessions still arrive, with the restore that
    // opens it.
    import_without_opening(&fixture, file, "transcript");
    restore_with_user_files(&fixture, "transcript");
    assert_eq!(
        fs::read_to_string(project.join("moved-session.jsonl")).unwrap(),
        "{\"turn\":1}\n"
    );
    assert_eq!(
        fs::read_to_string(project.join("moved-session/subagents/a.jsonl")).unwrap(),
        "sub\n"
    );
    assert!(!project.join("other-session.jsonl").exists());
    let output = fixture
        .command()
        .current_dir(path)
        .args(["resume", "--focus"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(path.join("resumed")).unwrap(),
        "moved-session"
    );
}

#[test]
fn a_chat_moves_with_its_session_and_continues_it() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './chat-agent.sh']\n",
    ));
    let home = fixture.root.path();
    // The agent reports its session as Claude Code's hook does, then talks
    // until a signal ends it; resumed, it only records its arguments.
    fs::write(
        fixture.repo.join("chat-agent.sh"),
        format!(
            "printf '%s\\n' \"$@\" > \"$HOME/chat-args\"\n\
             case \" $* \" in *' --resume '*) exit 0;; esac\n\
             printf '{{\"session_id\":\"chat-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n\
             trap 'exit 0' TERM\n\
             touch \"$HOME/chat-started\"\n\
             while :; do sleep 1; done\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    let projects = home.join(".claude/projects");
    let project = projects.join("-repo");
    let repo = fixture.repo.to_str().unwrap();
    let file = home.join("talk.shoal");
    let file = file.to_str().unwrap();
    std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            fixture.interactive_command(
                fixture.command().current_dir(&fixture.repo).args([
                    "start", "--chat", "--here", "--agent", "claude", "--name", "talk",
                ]),
                "",
            )
        });
        wait_until("chat started", || home.join("chat-started").exists());
        let chats = fixture.ok(&["list", "--chats"]);
        assert_eq!(chats[0]["session"], "chat-session", "{chats}");
        fs::create_dir_all(&project).unwrap();
        let transcript = project.join("chat-session.jsonl");
        fs::write(&transcript, "{\"turn\":1}\n").unwrap();
        let status = fixture.ok(&["status", "talk"]);
        assert_eq!(
            (
                &status["transcripts"][0]["path"],
                &status["transcripts"][0]["current"]
            ),
            (&serde_json::json!(transcript), &serde_json::json!(true)),
            "{status}"
        );

        // Run inside the chat without a target, the export never takes the
        // chat it would end under itself.
        let inside = fixture
            .command()
            .env("SHOAL_CHAT", chats[0]["id"].as_str().unwrap())
            .args(["move", "export", "-o", file, "--yes"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!inside.status.success(), "{inside:?}");
        assert_eq!(fixture.ok(&["list", "--chats"])[0]["name"], "talk");

        // Ending a chat the user talks with needs a confirmation.
        let unconfirmed = fixture.run(&["move", "export", "talk", "-o", file]);
        assert!(!unconfirmed.status.success(), "{unconfirmed:?}");
        assert_eq!(fixture.ok(&["list", "--chats"])[0]["name"], "talk");
        let exported = fixture.ok(&["move", "export", "talk", "-o", file, "--yes"]);
        assert_eq!(exported["manifest"]["kind"], "chat", "{exported}");
        assert_eq!(exported["manifest"]["name"], "talk", "{exported}");
        assert_eq!(
            exported["manifest"]["session_files"][0]["path"], "-repo/chat-session.jsonl",
            "{exported}"
        );
        let (output, transcript) = chat.join().unwrap();
        assert!(output.status.success(), "{output:?}\n{transcript}");
    });
    assert_eq!(fixture.ok(&["list", "--chats"]), serde_json::json!([]));

    // The importing machine has no transcript; the import puts it back and
    // the chat continues its session under its name, free again.
    fs::remove_dir_all(&projects).unwrap();
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .env("SHOAL_DEV_USER_FILES", "1")
            .args(["move", "import", file, "--repo", repo]),
        "",
    );
    assert!(output.status.success(), "{output:?}\n{transcript}");
    let args = fs::read_to_string(home.join("chat-args")).unwrap();
    assert!(args.contains("\n--resume\nchat-session\n"), "{args}");
    assert_eq!(
        fs::read_to_string(project.join("chat-session.jsonl")).unwrap(),
        "{\"turn\":1}\n"
    );
}

#[test]
fn the_sessions_of_a_profile_only_the_workspace_defines_move_with_it() {
    let fixture = Fixture::with_config(Some("[agents.claude]\ncommand = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("profiled");
    let path = Path::new(workspace["path"].as_str().unwrap());
    // Only the workspace's own config, which its removal deletes, names the
    // profile.
    fs::write(
        path.join(".shoal.toml"),
        "[agents.claude-moving]\nbase = 'claude'\n",
    )
    .unwrap();
    fs::write(
        path.join("agent.sh"),
        format!(
            "printf '{{\"session_id\":\"profiled-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["claude-moving", "profiled"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let project = fixture.root.path().join(".claude/projects/-elsewhere");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("profiled-session.jsonl"), "{}\n").unwrap();

    let file = fixture.root.path().join("profiled.shoal");
    let file = file.to_str().unwrap();
    let exported = fixture.ok(&["move", "export", "profiled", "-o", file]);
    assert_eq!(
        exported["manifest"]["session_files"][0]["path"], "-elsewhere/profiled-session.jsonl",
        "{exported}"
    );

    // The import that cannot open the workspace cannot read its config
    // either; the restore that opens it puts the session's file in place.
    fs::remove_dir_all(&project).unwrap();
    import_without_opening(&fixture, file, "profiled");
    assert!(!project.join("profiled-session.jsonl").exists());
    restore_with_user_files(&fixture, "profiled");
    assert_eq!(
        fs::read_to_string(project.join("profiled-session.jsonl")).unwrap(),
        "{}\n"
    );
    // Placed, the daemon's kept copies go.
    let kept = path.parent().unwrap().join(".sessions");
    let left: Vec<_> = fs::read_dir(&kept)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| entry.is_dir())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_restore_that_places_no_session_files_keeps_them_for_the_next() {
    let fixture = Fixture::with_config(Some("[agents.claude]\ncommand = ['sh', './agent.sh']\n"));
    let workspace = fixture.add("unplaced");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join("agent.sh"),
        format!(
            "printf '{{\"session_id\":\"unplaced-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    let output = fixture
        .command()
        .args(["claude", "unplaced"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let project = fixture.root.path().join(".claude/projects/-elsewhere");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("unplaced-session.jsonl"), "{}\n").unwrap();
    let file = fixture.root.path().join("unplaced.shoal");
    let file = file.to_str().unwrap();
    fixture.ok(&["move", "export", "unplaced", "-o", file]);
    fs::remove_dir_all(&project).unwrap();
    import_without_opening(&fixture, file, "unplaced");

    // A development build leaves the agents' files to the installed Shoal,
    // so the daemon keeps its copy.
    let restored = fixture
        .command()
        .env_remove("SHOAL_DEV_USER_FILES")
        .args(["--json", "restore", "unplaced"])
        .output()
        .unwrap();
    assert!(restored.status.success(), "{restored:?}");
    assert!(!project.join("unplaced-session.jsonl").exists());
    let kept = path.parent().unwrap().join(".sessions");
    let left: Vec<_> = fs::read_dir(&kept)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| entry.is_dir())
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");

    // The workspace's next removal record lists them, so the next restore
    // puts them in place.
    fixture.ok(&["rm", "--wait", "unplaced", "--yes"]);
    restore_with_user_files(&fixture, "unplaced");
    assert_eq!(
        fs::read_to_string(project.join("unplaced-session.jsonl")).unwrap(),
        "{}\n"
    );
}

/// Import the export `file` while a branch named `branch`, the workspace's,
/// has another history, so the import records the workspace without opening
/// it, then delete that branch. A development build leaves agents' files to
/// the installed Shoal unless told otherwise.
fn import_without_opening(fixture: &Fixture, file: &str, branch: &str) {
    let tree = git(&fixture.repo, &["write-tree"]);
    let unrelated = git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit-tree",
            tree.trim(),
            "-m",
            "Another history",
        ],
    );
    git(&fixture.repo, &["branch", "-f", branch, unrelated.trim()]);
    let repo = fixture.repo.to_str().unwrap();
    let imported = fixture
        .command()
        .env("SHOAL_DEV_USER_FILES", "1")
        .args(["--json", "move", "import", file, "--repo", repo])
        .output()
        .unwrap();
    assert!(!imported.status.success(), "{imported:?}");
    git(&fixture.repo, &["branch", "-D", branch]);
}

/// Restore the removed `workspace`, letting the development build put its
/// agents' session files in place.
fn restore_with_user_files(fixture: &Fixture, workspace: &str) {
    let restored = fixture
        .command()
        .env("SHOAL_DEV_USER_FILES", "1")
        .args(["--json", "restore", workspace])
        .output()
        .unwrap();
    assert!(restored.status.success(), "{restored:?}");
}

#[test]
fn exporting_a_parent_needs_its_children_exported_with_it() {
    let fixture = Fixture::new();
    let parent = PathBuf::from(fixture.add("parent")["path"].as_str().unwrap());
    let child = fixture.ok(&["split", "child", "--workspace", "parent"]);
    let child = PathBuf::from(child["path"].as_str().unwrap());
    let directory = fixture.root.path().join("exports");
    fs::create_dir(&directory).unwrap();
    let output = directory.to_str().unwrap();
    let file = directory.join("parent.shoal");
    // Every path refuses before removing anything.
    for args in [
        &[
            "move",
            "export",
            "parent",
            "-o",
            file.to_str().unwrap(),
            "--yes",
        ][..],
        &["move", "export", "parent", "-o", output, "--yes"],
    ] {
        let refused = fixture.run(args);
        assert!(!refused.status.success(), "{args:?}");
        assert!(parent.exists() && child.exists());
    }
    // Exported together, the child comes from the checkpoint its parent's
    // removal kept, whichever goes first.
    let exported = fixture.ok(&["move", "export", "parent", "child", "-o", output, "--yes"]);
    assert_eq!(exported["failed"], serde_json::json!([]), "{exported}");
    for name in ["parent", "child"] {
        assert!(directory.join(format!("{name}.shoal")).exists(), "{name}");
    }
    assert!(!parent.exists() && !child.exists());
}
