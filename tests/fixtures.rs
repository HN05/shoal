mod support;

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};
use support::{home::Home, printed};

#[test]
fn installed_scripts_preserve_literal_paths_contents_and_modes() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let script = root.path().join("nested ' $literal/bin/test script");
    let contents = "#!/bin/sh\nprintf '%s' \"$1\"\n";
    support::install_test_script(&script, contents);
    assert_eq!(fs::read_to_string(&script).unwrap(), contents);
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let output = support::isolated(root.path(), &script)
        .arg("quote ' ; $(touch injected)")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(output.stdout, b"quote ' ; $(touch injected)");
    assert!(!root.path().join("injected").exists());

    // An executing fixture keeps the previous file even after replacement.
    let mut previous = fs::File::open(&script).unwrap();
    support::install_test_script_with_mode(&script, "#!/bin/sh\nexit 7\n", 0o700);
    let mut previous_contents = String::new();
    previous.read_to_string(&mut previous_contents).unwrap();
    assert_eq!(previous_contents, contents);
    assert_eq!(
        previous.metadata().unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        support::isolated(root.path(), &script)
            .status()
            .unwrap()
            .code(),
        Some(7)
    );
}

/// A fixture's home stops, as it goes, the processes left in it, though
/// each leads a group of its own: one working in it and one naming it, but
/// no other.
#[test]
fn a_home_stops_the_processes_working_in_or_naming_it() {
    let home = Home::new();
    let elsewhere = tempfile::tempdir_in("/tmp").unwrap();
    let mut working = looping(home.path(), elsewhere.path());
    let mut naming = looping(elsewhere.path(), &home.path().join("state"));
    let mut other = looping(elsewhere.path(), elsewhere.path());

    drop(home);

    assert!(working.try_wait().unwrap().is_some());
    assert!(naming.try_wait().unwrap().is_some());
    assert!(other.try_wait().unwrap().is_none());
    // SAFETY: kill only sends a signal, to the group `other` leads.
    unsafe { libc::kill(-(other.id() as i32), libc::SIGKILL) };
    other.wait().unwrap();
}

/// A home whose test process is gone, as when a signal ended the test
/// before its drops ran, goes with the processes left in it.
#[test]
fn an_abandoned_home_goes_with_its_processes() {
    let mut ended = Command::new("true").spawn().unwrap();
    ended.wait().unwrap();
    let abandoned = Path::new("/tmp").join(format!(".t{}.test", ended.id()));
    fs::create_dir(&abandoned).unwrap();
    let elsewhere = tempfile::tempdir_in("/tmp").unwrap();
    let mut working = looping(&abandoned, elsewhere.path());
    let mut naming = looping(elsewhere.path(), &abandoned.join("state"));
    let kept = Home::new();

    support::home::remove_abandoned();

    assert!(!abandoned.exists());
    assert!(working.try_wait().unwrap().is_some());
    assert!(naming.try_wait().unwrap().is_some());
    assert!(kept.path().exists());
}

/// A test process that a signal ends before its drops run leaves nothing
/// running in its home, which goes too, though no later test sweeps it.
#[test]
fn a_killed_test_process_leaves_nothing_in_its_home() {
    let mut test = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "killed_test_process", "--ignored", "--nocapture"])
        .env(KILLED, "1")
        .stdout(Stdio::piped())
        // The loop it leaves keeps none of the runner's output open.
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (looping, home) = BufReader::new(test.stdout.take().unwrap())
        .lines()
        .find_map(|line| {
            let line = line.unwrap();
            let (pid, home) = line.split_once(' ')?;
            Some((pid.parse::<u32>().ok()?, PathBuf::from(home)))
        })
        .expect("the test process printed what runs in its home");

    test.kill().unwrap();
    test.wait().unwrap();

    support::wait::wait_until("the killed test's home to go", || {
        !home.exists() && !runs(looping)
    });
}

/// Set for the test process [`a_killed_test_process_leaves_nothing_in_its_home`] kills.
const KILLED: &str = "SHOAL_FIXTURE_KILLED";

/// Make a home and loop in it, printing the loop's ID and the home's path,
/// until killed.
#[test]
#[ignore = "the test process a_killed_test_process_leaves_nothing_in_its_home kills"]
fn killed_test_process() {
    if std::env::var_os(KILLED).is_none() {
        return;
    }
    let home = Home::new();
    // Killed, this process never reaps the loop.
    let looping = looping(home.path(), home.path()).id();
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{looping} {}", home.path().display()).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(600));
}

/// Whether process `pid` runs, other than as a zombie.
fn runs(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&output.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// A shell looping in `directory` with `argument` on its command line, in a
/// process group of its own.
fn looping(directory: &Path, argument: &Path) -> Child {
    Command::new("sh")
        .current_dir(directory)
        .args(["-c", "while :; do sleep 1; done", "sh"])
        .arg(argument)
        .process_group(0)
        .spawn()
        .unwrap()
}

#[test]
fn a_command_running_at_its_hang_guard_fails_naming_it() {
    let error = support::run::try_output_within(
        Command::new("sleep").arg("600"),
        std::time::Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("`sleep 600`"), "{error}");
}

#[test]
fn processes_ignore_launching_environment_and_allow_explicit_overrides() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("config.toml"), "invalid config").unwrap();
    // Pollute a subprocess running the probe, never the parallel test runner.
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "isolation_probe", "--nocapture"])
        .env("SHOAL_FIXTURE_PROBE", root.path())
        .env("HOME", &outside)
        .env("SHOAL_STATE_DIR", &outside)
        .env("SHOAL_SCOPE_TOKEN", "inherited-scope")
        .env("SHOAL_EXECUTION_ID", "inherited-execution")
        .env("SHOAL_COMPLETE", "bash")
        .env("SHOAL_FUTURE_SETTING", "inherited")
        .env("XDG_CONFIG_HOME", &outside)
        .env("XDG_STATE_HOME", &outside)
        .env("CLAUDE_CONFIG_DIR", &outside)
        .env("CODEX_HOME", &outside)
        .env("HAPPY_HOME_DIR", &outside)
        .env("HAPPY_SERVER_URL", "http://invalid")
        .env("GIT_DIR", &outside)
        .env("GIT_CONFIG_GLOBAL", outside.join("config.toml"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(fs::read_dir(outside).unwrap().count(), 1);
}

#[test]
fn isolation_probe() {
    let Some(root) = std::env::var_os("SHOAL_FIXTURE_PROBE") else {
        return;
    };
    let root = std::path::Path::new(&root);
    let output = support::isolated(root, "sh")
        .args(["-c", "env"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let environment = String::from_utf8(output.stdout).unwrap();
    for name in [
        "SHOAL_SCOPE_TOKEN",
        "SHOAL_EXECUTION_ID",
        "SHOAL_COMPLETE",
        "SHOAL_FUTURE_SETTING",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "HAPPY_HOME_DIR",
        "HAPPY_SERVER_URL",
        "GIT_DIR",
    ] {
        assert!(
            !environment
                .lines()
                .any(|line| line.starts_with(&format!("{name}="))),
            "{name}"
        );
    }
    assert!(
        environment
            .lines()
            .any(|line| line == format!("HOME={}", root.display()))
    );
    assert!(
        environment
            .lines()
            .any(|line| line == format!("SHOAL_STATE_DIR={}/state", root.display()))
    );
    assert_eq!(support::cli(root).get_current_dir(), Some(root));
    let output = support::cli(root)
        .args(["config", "install", "default"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", printed(&output));
    let output = support::cli(root)
        .env("SHOAL_SCOPE_TOKEN", "explicit-scope")
        .args(["config", "install", "default"])
        .output()
        .unwrap();
    // Scoped, it may not replace the config, which no check could hold to
    // the permissions its space holds.
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("key by key"));
    assert!(!root.join("state").exists());
}
