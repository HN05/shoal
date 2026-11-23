//! Unit-test fixtures; keep their state and repositories under the returned root.
use std::{
    ffi::{OsStr, OsString},
    fmt, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub use self::repositories::{
    commit, git_with_identity, reftable_repository, repository, repository_with, set_config,
};
use crate::{
    paths::Paths,
    process::identity::{self, Identity},
};

mod repositories;
pub mod subvolumes;
#[cfg(test)]
mod tests;

/// The prefixes of the runner's variables that would point a test's command
/// at the runner's Shoal, terminal and its hosts, agents, their provider or
/// Git repository.
const RUNNER_PREFIXES: [&str; 12] = [
    "SHOAL_",
    "XDG_",
    "HAPPY_",
    "GIT_",
    "HERDR_",
    "ZMX_",
    "REX_",
    "TERM",
    "TMUX",
    "ZELLIJ",
    "CLAUDE",
    "ANTHROPIC_",
];

/// The runner's variables beyond [`RUNNER_PREFIXES`] that would point a
/// test's command at its agents' configuration or prompt cache, or a shell's
/// startup files.
const RUNNER_VARIABLES: [&str; 5] = [
    "CODEX_HOME",
    "ENABLE_PROMPT_CACHING_1H",
    "FORCE_PROMPT_CACHING_5M",
    "BASH_ENV",
    "ENV",
];

/// Run `command` from `root`, its home, without the runner's Shoal, terminal,
/// agent or Git variables, Git configuration beyond the repository's, a
/// repository above the [`git_ceilings`], or stdin: a command that read an open pipe or socket to its end would
/// wait forever. The rest of the environment, such as `PATH` and `TMPDIR`,
/// stays.
pub fn isolate<'a>(command: &'a mut Command, root: &Path) -> &'a mut Command {
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| {
            RUNNER_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
                || RUNNER_VARIABLES.contains(&name)
        }) {
            command.env_remove(name);
        }
    }
    command
        .current_dir(root)
        .env("HOME", root)
        .env("ZDOTDIR", root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(GIT_CEILING, git_ceilings())
        .stdin(std::process::Stdio::null())
}

/// The variable naming the directories Git looks for a repository no
/// further up than.
const GIT_CEILING: &str = "GIT_CEILING_DIRECTORIES";

/// This process's [`GIT_CEILING`] with the temporary directories tests make
/// their repositories in, `TMPDIR` and `/tmp`, so that a repository a stray
/// command made above them, such as in the repository directory that holds
/// a Shoal runner's `TMPDIR`, never takes in a test's directory.
fn git_ceilings() -> OsString {
    let inherited = std::env::var_os(GIT_CEILING).unwrap_or_default();
    // An empty entry would stop Git resolving the symlinks of those after it.
    let mut ceilings: Vec<_> = std::env::split_paths(&inherited)
        .filter(|ceiling| !ceiling.as_os_str().is_empty())
        .collect();
    for temporary in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        if !ceilings.contains(&temporary) {
            ceilings.push(temporary);
        }
    }
    std::env::join_paths(ceilings).unwrap()
}

/// Stop Git, run by this process or what it starts, and Shoal's reads from
/// looking for a repository above the [`git_ceilings`].
///
/// # Safety
/// Call before any other thread exists.
pub unsafe fn stop_git_at_temporary_directories() {
    // SAFETY: the caller guarantees no other thread reads the environment.
    unsafe { std::env::set_var(GIT_CEILING, git_ceilings()) };
}

/// On macOS, the directory of the programs that `/usr/bin`'s developer
/// tool shims, such as `git` and `python3`, run: a shim looks the developer
/// directory up on every run, which nearly doubles what a Git command
/// costs, and tests run Git many times each.
pub fn developer_tools() -> Option<&'static Path> {
    static TOOLS: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    let find = || {
        let output = Command::new("xcode-select").arg("-p").output().ok()?;
        let developer = String::from_utf8(output.stdout).ok()?;
        let tools = Path::new(developer.trim()).join("usr/bin");
        tools.join("git").exists().then_some(tools)
    };
    let tools = TOOLS.get_or_init(|| cfg!(target_os = "macos").then(find).flatten());
    tools.as_deref()
}

/// `path` with the [`developer_tools`] ahead of `/usr/bin`, so that what it
/// finds there runs without its shim.
pub fn with_developer_tools(path: &OsStr) -> OsString {
    let directories = std::env::split_paths(path).flat_map(|directory| {
        let tools = (directory == Path::new("/usr/bin")).then(developer_tools);
        tools
            .flatten()
            .map(Path::to_path_buf)
            .into_iter()
            .chain([directory])
    });
    std::env::join_paths(directories).unwrap()
}

/// Put the [`developer_tools`] ahead of `/usr/bin` on this process's `PATH`,
/// so that the code under test, and what it starts, runs Git without the
/// shim.
///
/// # Safety
/// Call before any other thread exists.
pub unsafe fn run_developer_tools_directly() {
    if developer_tools().is_none() {
        return;
    }
    let path = with_developer_tools(&std::env::var_os("PATH").unwrap_or_default());
    // SAFETY: the caller guarantees no other thread reads the environment.
    unsafe { std::env::set_var("PATH", path) };
}

/// The Git tests run: the [`developer_tools`]' own where there are any.
fn git_program() -> PathBuf {
    developer_tools().map_or_else(|| "git".into(), |tools| tools.join("git"))
}

/// Run Git in `repo` in the environment [`isolate`] gives it.
pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = isolate(&mut Command::new(git_program()), repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git in {} {args:?}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// `output` as a failing assertion shows it: its exit status, then what the
/// command printed on stdout and stderr as text, where `Output`'s `Debug`
/// lists their bytes.
pub fn printed(output: &Output) -> Printed<'_> {
    Printed(output)
}

/// A command's [`Output`] shown as text; see [`printed`].
pub struct Printed<'a>(&'a Output);

impl fmt::Display for Printed<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Output {
            status,
            stdout,
            stderr,
        } = self.0;
        write!(
            formatter,
            "{status}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(stdout),
            String::from_utf8_lossy(stderr),
        )
    }
}

/// Global config `text` as a published config, read like the one
/// [`write_global_config`] writes.
pub fn global_config(text: &str) -> crate::config::Config {
    crate::config::Config::from_toml(&format!("{BASE_CONFIG}{text}")).unwrap()
}

/// Global config always turns off `copy_ignored`: only the CLI runs the
/// tracked setup it needs, so workspaces created here are ready at once. It
/// delivers each agent message at once, so a test reads it without waiting
/// for a run of them to settle.
const BASE_CONFIG: &str = "setup.copy_ignored = false\nalerts.settle_seconds = 0\n";

/// Write global config `text` after [`BASE_CONFIG`] for the state of `paths`.
pub fn write_global_config(paths: &Paths, text: &str) {
    write_beside_global_config(paths, "config.toml", &format!("{BASE_CONFIG}{text}"));
}

/// Write `text` to `name` in the directory of the global config for `paths`,
/// failing for the user's own config directory.
pub fn write_beside_global_config(paths: &Paths, name: &str, text: &str) {
    let config = crate::config::Config::path(paths);
    let path = config.parent().unwrap().join(name);
    crate::config_file::refuse_user_config(&path).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
}

/// Run a test helper as its own wrapper, without the runner's scope or terminal.
pub fn isolated_test(root: &Path, name: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    isolate(command.as_std_mut(), root);
    command
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env("SHOAL_TEST_HELPER", "1")
        // The helper runs CLI handlers in-process; give its test thread the
        // stack the CLI's runtime threads get, which Linux debug builds need.
        .env("RUST_MIN_STACK", crate::WORKER_STACK.to_string())
        .kill_on_drop(true);
    command
}

/// Run a test helper as [`isolated_test`] does, as the leader of a new
/// session whose controlling terminal is a new one, so it starts as that
/// terminal's foreground job.
pub async fn in_new_terminal(root: &Path, name: &str) -> std::process::Output {
    use std::os::fd::FromRawFd;

    let (mut master, mut slave) = (-1, -1);
    // SAFETY: openpty initializes both descriptors; File owns them below.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0);
    // SAFETY: openpty returned the descriptors, which nothing else owns.
    let (master, slave) = unsafe { (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave)) };
    let mut helper = isolated_test(root, name);
    helper.stdin(slave);
    // SAFETY: setsid and ioctl are async-signal-safe.
    unsafe {
        helper.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), helper.output())
        .await
        .expect("the test helper in a new terminal stalled")
        .unwrap();
    // The terminal stays until the helper is done with it.
    drop(master);
    output
}

/// Install an executable fixture, including any parent directories it needs.
pub fn install_test_script(path: impl AsRef<Path>, script: impl AsRef<[u8]>) {
    install_test_script_with_mode(path, script, 0o755);
}

/// Install an executable fixture with an explicit Unix permission mode,
/// renamed over any earlier one, which may still be running.
pub fn install_test_script_with_mode(path: impl AsRef<Path>, script: impl AsRef<[u8]>, mode: u32) {
    use std::{io::Write as _, os::unix::fs::PermissionsExt as _};
    let path = std::path::absolute(path).unwrap();
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    // This process writes the staged file but never runs it.
    let mut staged = tempfile::NamedTempFile::new_in(parent).unwrap();
    staged.write_all(script.as_ref()).unwrap();
    fs::set_permissions(staged.path(), fs::Permissions::from_mode(mode)).unwrap();
    copy_in_child(staged.path(), &path);
}

/// Copy `source` to a fresh file with its permissions, which a child process
/// writes, and rename that over `destination`. A test that forks while this
/// binary holds a file open for writing leaves the descriptor in its child
/// until the child execs, and running the file meanwhile fails with "Text
/// file busy".
pub fn copy_in_child(source: &Path, destination: &Path) {
    let parent = destination.parent().unwrap();
    let staging = tempfile::tempdir_in(parent).unwrap();
    let fresh = staging.path().join("copy");
    let output = isolate(&mut Command::new("cp"), parent)
        .arg(source)
        .arg(&fresh)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "copy {} to {}: {}",
        source.display(),
        destination.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::set_permissions(&fresh, fs::metadata(source).unwrap().permissions()).unwrap();
    fs::rename(&fresh, destination).unwrap();
}

/// Whether the filesystem holding `dir` clones files without copying them.
/// The probe calls the system directly instead of Shoal's clone code, so a
/// broken clone never passes for a filesystem without cloning, and any failure
/// other than missing support fails the test.
pub fn native_cloning(dir: &Path) -> bool {
    let probe = tempfile::tempdir_in(dir).unwrap();
    let source = probe.path().join("source");
    fs::write(&source, "probe").unwrap();
    match clone_file(&source, &probe.path().join("clone")) {
        Ok(()) => true,
        Err(error) if cloning_unsupported(&error) => false,
        Err(error) => panic!("probe cloning in {}: {error}", dir.display()),
    }
}

#[cfg(target_os = "macos")]
fn clone_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};
    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: both arguments are NUL-terminated paths that outlive the call.
    match unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

#[cfg(target_os = "macos")]
fn cloning_unsupported(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOTSUP)
}

#[cfg(target_os = "linux")]
fn clone_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let source = fs::File::open(source)?;
    let destination = fs::File::create(destination)?;
    // SAFETY: both descriptors stay open for the call.
    match unsafe { libc::ioctl(destination.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// The errors filesystems without reflinks give, as coreutils' `cp` reads
/// them. Containers refuse the ioctl with EPERM.
#[cfg(target_os = "linux")]
fn cloning_unsupported(error: &std::io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        [
            libc::EOPNOTSUPP,
            libc::ENOTTY,
            libc::ENOSYS,
            libc::EINVAL,
            libc::EXDEV,
            libc::EPERM,
        ]
        .contains(&code)
    })
}

/// The processes of this user, other than this one, that work in `root` or
/// a directory below it.
pub fn working_in(root: &Path) -> anyhow::Result<Vec<u32>> {
    let root = fs::canonicalize(root)?;
    Ok(crate::process::directories::working_directories()?
        .into_iter()
        .filter(|process| process.path.starts_with(&root))
        .map(|process| process.pid)
        .collect())
}

/// A shell command that runs until this test binary exits, for at most ten
/// minutes, standing in for a long-running tool: a test killed before it
/// stops the command leaves nothing running.
pub fn until_tests_end() -> String {
    format!(
        "i=0; while [ $i -lt 600 ] && kill -0 {} 2>/dev/null; do sleep 1; i=$((i+1)); done",
        std::process::id()
    )
}

/// Poll through setup and verify that an explicit gate still holds the future.
pub async fn assert_pending(future: impl std::future::Future) {
    tokio::pin!(future);
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "future completed before release"
        );
        std::task::Poll::Ready(())
    })
    .await;
}

/// A shell left running from `dir` after its parent exited, as a recording's
/// setup script is, with a `sleep` it started there. Dropping it kills both.
pub struct Detached {
    pub shell: Identity,
    pub child: Identity,
}

impl Detached {
    /// Start it, recording its PIDs in `pids`, which must lie outside `dir`.
    pub async fn start(dir: &Path, pids: &Path) -> Self {
        let script = format!(
            "sh -c 'sleep 300 & echo $$ $! > {pids}.tmp; mv {pids}.tmp {pids}; wait' \
             >/dev/null 2>&1 &",
            pids = pids.display()
        );
        let status = isolated_command("sh", dir)
            .args(["-c", &script])
            .status()
            .await
            .unwrap();
        assert!(status.success());
        Self::recorded(pids).await
    }

    /// The pair whose PIDs a shell started elsewhere records in `pids`,
    /// once it has.
    pub async fn recorded(pids: &Path) -> Self {
        // A generous hang guard; the shell writes the file once both run.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !pids.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "detached shell never started"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let text = fs::read_to_string(pids).unwrap();
        let mut identities = text
            .split_whitespace()
            .map(|pid| identity::capture(pid.parse().unwrap()).unwrap().unwrap());
        Self {
            shell: identities.next().unwrap(),
            child: identities.next().unwrap(),
        }
    }
}

impl Drop for Detached {
    fn drop(&mut self) {
        for process in [&self.child, &self.shell] {
            let _ = identity::signal(process, libc::SIGKILL);
        }
    }
}

/// A process started from a shell that exits at once, so launchd or init
/// adopts it. Dropping it kills it.
pub struct Adopted {
    pub identity: Identity,
}

impl Adopted {
    /// Run `command` in `dir` with `env`, recording its PID in `pid`, which
    /// must lie outside the directories a test looks for processes in.
    pub async fn start(dir: &Path, command: &str, env: &[(&str, &str)], pid: &Path) -> Self {
        let script = format!(
            "{command} >/dev/null 2>&1 & echo $! > {pid}.tmp; mv {pid}.tmp {pid}",
            pid = pid.display()
        );
        let status = isolated_command("sh", dir)
            .envs(env.iter().copied())
            .args(["-c", &script])
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let pid: u32 = fs::read_to_string(pid).unwrap().trim().parse().unwrap();
        let adopted = Self {
            identity: identity::capture(pid).unwrap().unwrap(),
        };
        adopted.await_exec(&script).await;
        adopted
    }

    /// Wait until the process runs its command rather than the shell that
    /// forked it, whose command line is `script`.
    async fn await_exec(&self, script: &str) {
        // A generous hang guard; the fork execs the command at once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let described = crate::process::described::describe(&[self.identity.pid]).unwrap();
            let command = &described[&self.identity.pid].command;
            if !command.ends_with(script) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{command} never ran its command"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Adopted {
    fn drop(&mut self) {
        let _ = identity::signal(&self.identity, libc::SIGKILL);
    }
}

/// Copy this test binary to `path`, as [`copy_in_child`] does.
pub fn copy_test_binary(path: &Path) {
    copy_in_child(&std::env::current_exe().unwrap(), path);
}

/// A command running `program`, a copy of this test binary, until stopped.
/// Its environment stays readable once launchd adopts it, which a platform
/// binary such as `sleep` hides on macOS.
pub fn runs_until_stopped(program: &Path) -> String {
    format!("'{}' {}", program.display(), RUN_UNTIL_STOPPED.join(" "))
}

/// This test binary running until stopped in `dir`, started without a shell,
/// so a survey never finds the shell, whose environment macOS hides, in its
/// place.
pub fn run_until_stopped_in(dir: &Path) -> tokio::process::Command {
    let program = std::env::current_exe().unwrap();
    let mut command = isolated_command(program.to_str().unwrap(), dir);
    command.args(RUN_UNTIL_STOPPED);
    command
}

/// The arguments that make this test binary, or a copy, run until stopped.
const RUN_UNTIL_STOPPED: [&str; 3] = ["--exact", "helper_tests::run_until_stopped", "--ignored"];

/// The ignored tests that the helpers above and below start in this test
/// binary. Each crate whose tests use those helpers defines them as the
/// `helper_tests` module at its root.
#[macro_export]
macro_rules! helper_tests {
    () => {
        #[test]
        #[ignore = "helper process tests start"]
        fn run_until_stopped() {
            std::thread::sleep(std::time::Duration::from_secs(300));
        }

        #[test]
        #[ignore = "helper child launched by ownership tests"]
        fn marked_child() {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    };
}

/// Prepare this test binary's environment before `main`: forget the
/// repository variables a Git hook or `git rebase --exec` running it
/// exports, since the code under test starts Git, and scripts that run Git,
/// which would otherwise work in that repository instead of the fixtures';
/// [`stop_git_at_temporary_directories`]; and
/// [`run_developer_tools_directly`]. Each crate whose tests start such
/// processes expands it at its root.
#[macro_export]
macro_rules! prepare_test_process {
    () => {
        #[used]
        #[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
        #[cfg_attr(
            target_vendor = "apple",
            unsafe(link_section = "__DATA,__mod_init_func")
        )]
        static PREPARE_TEST_PROCESS: extern "C" fn() = {
            extern "C" fn prepare() {
                // SAFETY: constructors run before `main` starts any thread.
                unsafe {
                    $crate::git::forget_inherited_repository();
                    $crate::test_support::stop_git_at_temporary_directories();
                    $crate::test_support::run_developer_tools_directly();
                }
            }
            prepare
        };
    };
}

/// This test binary's `marked_child` helper, which waits 30 seconds, for a
/// test to give its environment before [`spawn_marked_child`] starts it.
/// It carries no Shoal identity but the one the test gives it.
pub fn marked_child() -> tokio::process::Command {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "helper_tests::marked_child", "--ignored"]);
    without_runner_identity(command.as_std_mut());
    command
}

/// `program` without the identity of a workspace or chat the runner may run
/// in, which would make what it starts theirs rather than the test's spaces'.
pub fn unmarked(program: &str) -> Command {
    let mut command = Command::new(program);
    without_runner_identity(&mut command);
    command
}

/// The variables that tell Shoal which space or execution a process
/// belongs to, which a runner inside a workspace or chat carries.
const IDENTITY: [&str; 3] = [
    crate::env::EXECUTION_ID,
    crate::env::SPACE,
    crate::env::ONESHOT_RUN,
];

/// Remove from `command` the Shoal identity it would inherit from the
/// runner, keeping what the test set itself, so tests behave the same inside
/// and outside a workspace.
fn without_runner_identity(command: &mut Command) {
    let set: Vec<_> = command
        .get_envs()
        .map(|(name, _)| name.to_owned())
        .collect();
    for name in IDENTITY {
        if !set.iter().any(|set| set == name) {
            command.env_remove(name);
        }
    }
}

/// Start `command` from [`marked_child`] and return once the helper runs its
/// own image. Linux resumes a `posix_spawn` caller before the child leaves
/// the caller's memory, so until then the child's environment reads as the
/// caller's. The helper prints libtest's header only after that. The child
/// keeps its output's read end, so the helper's later writes still succeed.
pub fn spawn_marked_child(command: &mut tokio::process::Command) -> tokio::process::Child {
    use std::io::Read as _;
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap().into_owned_fd().unwrap();
    let mut stdout = std::process::ChildStdout::from(stdout);
    stdout.read_exact(&mut [0]).unwrap();
    child.stdout = Some(tokio::process::ChildStdout::from_std(stdout).unwrap());
    child
}

/// A process a test started as the leader of its own process group.
/// Dropping it kills the group, so neither the process nor what it started
/// outlives a test that fails or times out.
pub struct ProcessGroup {
    leader: std::process::Child,
    /// Once the leader is reaped, its ID may name another group.
    reaped: bool,
}

impl ProcessGroup {
    /// Start `command` without the runner's Shoal identity, keeping the one
    /// the test gave it.
    pub fn spawn(command: &mut Command) -> Self {
        use std::os::unix::process::CommandExt as _;
        without_runner_identity(command);
        Self {
            leader: command.process_group(0).spawn().unwrap(),
            reaped: false,
        }
    }

    pub fn id(&self) -> u32 {
        self.leader.id()
    }

    /// Wait for the leader to exit on its own or by another's signal.
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let status = self.leader.wait()?;
        self.reaped = true;
        Ok(status)
    }

    /// Kill the group and wait for its leader.
    pub fn kill(self) {
        drop(self);
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // SAFETY: the unreaped leader keeps its group's ID from naming another.
        unsafe {
            libc::kill(-(self.leader.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.leader.wait();
    }
}

/// `program` run from `dir` in the environment [`isolate`] gives it.
pub fn isolated_command(program: &str, dir: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    isolate(command.as_std_mut(), dir);
    command.kill_on_drop(true);
    command
}

/// The Zellij sessions whose servers run in `socket_dir`, by the names
/// they started with: Zellij 0.46 names its sockets by an ID.
pub fn zellij_sessions(socket_dir: &Path) -> std::io::Result<Vec<String>> {
    crate::sessions::zellij::served_in(socket_dir).map_err(std::io::Error::other)
}

/// Decline Zellij's setup in `home`: from 0.46 a new home opens a floating
/// setup wizard where Zellij finds a desktop, which refuses splits while it
/// shows and keeps a session whose panes have closed.
pub fn decline_zellij_setup(home: &Path) {
    for data in [
        ".local/share/zellij",
        "Library/Application Support/org.Zellij-Contributors.Zellij",
    ] {
        let data = home.join(data);
        fs::create_dir_all(&data).unwrap();
        fs::write(
            data.join("desktop_integration.json"),
            r#"{"flags": ["SetupNeverShow"]}"#,
        )
        .unwrap();
    }
}

/// The message Shoal posted to a Happy session, decrypted from its posted
/// `content` with `key`, the account secret for the `legacy` encryption
/// variant or the session's key for `dataKey`, as Happy spells them.
pub fn happy_message(variant: &str, key: &[u8], content: &[u8]) -> serde_json::Value {
    let plaintext = match variant {
        "legacy" => {
            use crypto_secretbox::{
                XSalsa20Poly1305,
                aead::{Aead, KeyInit},
            };
            let (nonce, ciphertext) = content.split_at(24);
            XSalsa20Poly1305::new_from_slice(key)
                .unwrap()
                .decrypt(nonce.into(), ciphertext)
                .unwrap()
        }
        "dataKey" => {
            use aes_gcm::{
                Aes256Gcm,
                aead::{Aead, KeyInit},
            };
            let (version, bundle) = content.split_first().unwrap();
            assert_eq!(*version, 0);
            let (nonce, ciphertext) = bundle.split_at(12);
            Aes256Gcm::new_from_slice(key)
                .unwrap()
                .decrypt(nonce.try_into().unwrap(), ciphertext)
                .unwrap()
        }
        _ => panic!("unknown Happy encryption variant {variant}"),
    };
    serde_json::from_slice(&plaintext).unwrap()
}
