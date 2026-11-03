//! The Zellij CLI against the sessions of one socket directory, for the
//! terminal host and the daemon's cleanup alike. Zellij runs a server per
//! session and numbers its panes afresh each time one starts, so a place
//! names its session beside the pane and records the server's instance, its
//! process and birth: a pane under a recorded ID is the recorded one only on
//! the server instance that created it. Zellij takes no condition with a
//! command, so Shoal reads the instance just before each command on a
//! recorded pane.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Output,
    time::Duration,
};

use anyhow::{Context as _, Result, bail, ensure};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use tokio::process::Command;

use crate::{env, subprocess::Run, tools::Tool};

/// What a Zellij origin's data records: the socket directory of its
/// session's server.
const SOCKET_DIR: &str = "socket_dir";
/// The plugins Zellij's layouts put in every tab, which are no pane of the
/// user's.
const LAYOUT_PLUGINS: &[&str] = &["tab-bar", "status-bar", "compact-bar"];
/// The oldest Zellij Shoal supports, the first that opens tabs and panes
/// without focusing them.
const OLDEST: (u32, u32) = (0, 45);
/// The longest name Shoal gives a session, whose socket path is short on
/// macOS; the session's tab shows the whole label.
const MAX_SESSION_LEN: usize = 20;
/// The longest socket path macOS takes, and what Zellij puts between the
/// socket directory and the session's name.
const MAX_SOCKET_PATH: usize = 103;
const CONTRACT_DIR: &str = "/contract_version_1/";
/// How often, and how many times, Shoal reads a new tab's, split's or
/// session's panes until its pane shows; only a hang guard, since Zellij lists it
/// soon after it answers.
const PANE_POLL: Duration = Duration::from_millis(100);
const PANE_POLLS: u32 = 100;
/// How often, and how many times, Shoal asks Zellij again about a session it
/// reports as missing while its server still runs; only a hang guard, since
/// Zellij finds the session once its server answers in time.
const MISSING_RETRY: Duration = Duration::from_millis(100);
const MISSING_ATTEMPTS: u32 = 100;

/// How long a `zellij` command may run before it is stopped; only a hang
/// guard, since each one answers at once.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The variable choosing the directory of every session's socket.
pub(crate) const SOCKET_DIR_VAR: &str = "ZELLIJ_SOCKET_DIR";
/// The variable naming the session a pane runs in.
pub(crate) const SESSION_VAR: &str = "ZELLIJ_SESSION_NAME";
/// The variable naming the pane a process runs in, by its number.
pub(crate) const PANE_VAR: &str = "ZELLIJ_PANE_ID";
/// The variables Zellij sets in each pane, which name its place and keep a
/// client from starting inside it.
pub(crate) const PLACE_VARS: &[&str] = &["ZELLIJ", SESSION_VAR, PANE_VAR];
/// The variables that choose the sessions `zellij` reaches.
pub(crate) const SERVER_VARS: &[&str] = &[SOCKET_DIR_VAR];

/// The sessions of one socket directory; `None` is the one `zellij` reaches
/// by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Zellij {
    socket_dir: Option<PathBuf>,
}

/// A pane of a session, as a place's ID names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneRef<'a> {
    pub session: &'a str,
    /// The pane, such as `terminal_3`.
    pub pane: &'a str,
}

/// A pane and where it sits now: a terminal, or a plugin the user opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pane {
    /// Such as `terminal_3` or `plugin_2`.
    pub id: String,
    pub tab: u64,
    pub tab_name: String,
    /// What runs in its foreground, as Zellij reads it.
    pub command: Option<String>,
    /// Whether its command exited and Zellij holds the pane open.
    pub exited: bool,
}

/// A pane Shoal opened, with the session, server instance and socket
/// directory it opened in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opened {
    pub session: String,
    pub pane: String,
    pub instance: String,
    pub socket_dir: PathBuf,
}

/// A pane to open: in a tab of its own, beside a pane, or in a session of
/// its own.
pub(crate) struct Shell<'a> {
    pub label: &'a str,
    pub cwd: &'a Path,
    pub focus: bool,
    /// What the pane runs, without a shell, closing the pane once it ends;
    /// empty runs the default shell.
    pub command: &'a [String],
}

/// A session's server: its instance and the socket directory it listens in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Server {
    pid: u32,
    socket_dir: PathBuf,
}

/// What `list-panes --json` reports for a pane.
#[derive(Deserialize)]
struct Listed {
    id: u64,
    is_plugin: bool,
    #[serde(default)]
    is_suppressed: bool,
    #[serde(default)]
    plugin_url: Option<String>,
    tab_id: u64,
    #[serde(default)]
    tab_name: String,
    #[serde(default)]
    exited: bool,
    #[serde(default)]
    pane_command: Option<String>,
}

impl Pane {
    /// Whether the pane is a terminal rather than a plugin.
    pub fn is_terminal(&self) -> bool {
        self.id.starts_with("terminal_")
    }
}

impl Zellij {
    /// The sessions of the socket directory `data` records; without one,
    /// [`Zellij::reaching`] finds it from the session's server, whatever
    /// this process's own directory.
    pub fn recorded(data: &BTreeMap<String, String>) -> Self {
        Self {
            socket_dir: data.get(SOCKET_DIR).map(PathBuf::from),
        }
    }

    /// The sessions this process reaches.
    pub fn here() -> Self {
        Self {
            socket_dir: var(SOCKET_DIR_VAR).map(PathBuf::from),
        }
    }

    /// What an origin records to reach a session in `socket_dir`.
    pub fn data(socket_dir: &Path) -> BTreeMap<String, String> {
        BTreeMap::from([(
            SOCKET_DIR.to_owned(),
            socket_dir.to_string_lossy().into_owned(),
        )])
    }

    /// These sessions, in the socket directory of the server of `session`
    /// where none was chosen, so commands on a place recorded without one
    /// reach its server whatever this process's default directory.
    pub async fn reaching(self, session: &str) -> Result<Self> {
        if self.socket_dir.is_some() {
            return Ok(self);
        }
        Ok(match self.server(session).await? {
            Some(server) => Self {
                socket_dir: Some(server.socket_dir),
            },
            None => self,
        })
    }

    /// The socket directory these sessions listen in, where the caller or
    /// a record chose one.
    pub fn socket_dir(&self) -> Option<&Path> {
        self.socket_dir.as_deref()
    }

    /// Whether `other` names the sessions of this socket directory, as
    /// [`Zellij::reaching`] resolves both.
    pub fn same_sessions(&self, other: &Self) -> bool {
        match (&self.socket_dir, &other.socket_dir) {
            (Some(mine), Some(theirs)) => same_dir(mine, theirs),
            (mine, theirs) => mine == theirs,
        }
    }

    /// A `zellij` command for these sessions, without the variables that
    /// name the caller's own pane. An empty socket directory, as a start
    /// queued from no terminal carries, is unset.
    fn command(&self) -> Command {
        let mut command = Command::new(Tool::Zellij.program());
        command.stdin(std::process::Stdio::null());
        for name in PLACE_VARS.iter().chain(SERVER_VARS) {
            command.env_remove(name);
        }
        if let Some(dir) = self.socket_dir.as_deref() {
            command.env(SOCKET_DIR_VAR, dir);
        }
        command
    }

    /// A `zellij action` command on `session`.
    fn action(&self, session: &str) -> Command {
        let mut command = self.command();
        command.args(["--session", session, "action"]);
        command
    }

    /// Refuse a `zellij` older than [`OLDEST`].
    pub async fn ensure_supported(&self) -> Result<()> {
        let mut version = self.command();
        version.arg("--version");
        let version = run(version).await?;
        if let Some(found) = parse_version(&version) {
            ensure!(
                found >= OLDEST,
                "{} is too old; Shoal needs Zellij {}.{} or later",
                version.trim(),
                OLDEST.0,
                OLDEST.1
            );
        }
        Ok(())
    }

    /// The instance of the server of `session`, its process and birth, and
    /// the socket directory it listens in; none while no server runs it.
    pub async fn instance(&self, session: &str) -> Result<Option<(String, PathBuf)>> {
        let Some(server) = self.server(session).await? else {
            return Ok(None);
        };
        let Some(identity) = crate::process::identity::capture(server.pid)? else {
            return Ok(None);
        };
        Ok(Some((
            format!("{}:{}", identity.pid, identity.birth),
            server.socket_dir,
        )))
    }

    /// The server process of `session` in this socket directory, among this
    /// user's processes. Without a directory, the only one of that name.
    async fn server(&self, session: &str) -> Result<Option<Server>> {
        let mut ps = Command::new(Tool::Ps.program());
        ps.args(["-A", "-o", "uid=", "-o", "pid=", "-o", "args="])
            .stdin(std::process::Stdio::null());
        let listed = run(ps).await?;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let mut servers = listed
            .lines()
            .filter_map(|line| parse_server(line, uid, session))
            .filter(|server| {
                self.socket_dir
                    .as_deref()
                    .is_none_or(|dir| same_dir(dir, &server.socket_dir))
            });
        let server = servers.next();
        ensure!(
            servers.next().is_none(),
            "several Zellij sessions are named {session}; set {SOCKET_DIR_VAR}"
        );
        Ok(server)
    }

    /// Every pane of `session`; none when it is gone.
    pub async fn panes(&self, session: &str) -> Result<Vec<Pane>> {
        self.panes_on(session, None).await
    }

    /// Every pane of `session` while its server is the instance `instance`,
    /// or any instance without one; none once it is not.
    async fn panes_on(&self, session: &str, instance: Option<&str>) -> Result<Vec<Pane>> {
        let listed = self
            .session_listing(session, instance, &["list-panes", "--all", "--json"])
            .await?;
        parse_panes(&listed)
    }

    /// What the `zellij action` listing `args` prints for `session` while
    /// its server is the instance `instance`, or any instance without one;
    /// empty once it is not.
    async fn session_listing(
        &self,
        session: &str,
        instance: Option<&str>,
        args: &[&str],
    ) -> Result<String> {
        let listed = asked_while_running(
            || {
                let mut list = self.action(session);
                list.args(args);
                listing(list)
            },
            || self.runs(session, instance),
        )
        .await
        .with_context(|| format!("list Zellij session {session}"))?;
        Ok(listed.unwrap_or_default())
    }

    /// The pane `place`, while it is open on the server instance `instance`,
    /// or on any instance without one.
    pub async fn pane(&self, place: PaneRef<'_>, instance: Option<&str>) -> Result<Option<Pane>> {
        if let Some(instance) = instance
            && !self.on_instance(place.session, instance).await?
        {
            return Ok(None);
        }
        Ok(self
            .panes_on(place.session, instance)
            .await?
            .into_iter()
            .find(|pane| pane.id == place.pane))
    }

    /// Whether a server runs `session`: the instance `instance`, or any
    /// instance without one.
    async fn runs(&self, session: &str, instance: Option<&str>) -> Result<bool> {
        match instance {
            Some(instance) => self.on_instance(session, instance).await,
            None => Ok(self.server(session).await?.is_some()),
        }
    }

    /// Whether the server of `session` is the instance `instance`.
    async fn on_instance(&self, session: &str, instance: &str) -> Result<bool> {
        Ok(self
            .instance(session)
            .await?
            .is_some_and(|(running, _)| running == instance))
    }

    /// The pane `place` on the instance `instance`, which a command on it
    /// needs.
    async fn require(&self, place: PaneRef<'_>, instance: &str) -> Result<Pane> {
        self.pane(place, Some(instance))
            .await?
            .with_context(|| format!("the Zellij pane {} is gone", place.pane))
    }

    /// The pane each client attached to `session` shows.
    pub async fn clients(&self, session: &str) -> Result<Vec<String>> {
        let listed = self
            .session_listing(session, None, &["list-clients"])
            .await?;
        Ok(parse_clients(&listed))
    }

    /// Every session's name, exited ones included, which Zellij would
    /// resurrect rather than create under that name.
    async fn session_names(&self) -> Result<Vec<String>> {
        let mut list = self.command();
        list.args(["list-sessions", "--no-formatting"]);
        let listed = listing(list).await?.answer().unwrap_or_default();
        Ok(parse_sessions(&listed))
    }

    /// Open `shell` in a new tab of `session`, which a client shows: Zellij
    /// drops tabs opened in a session no client shows.
    pub async fn open_tab(&self, session: &str, shell: &Shell<'_>) -> Result<Opened> {
        let tab = run(self.new_tab(session, shell))
            .await
            .context("open a Zellij tab")?;
        let tab: u64 = tab.trim().parse().context("invalid Zellij tab")?;
        let pane = self
            .new_pane(session, |pane| pane.is_terminal() && pane.tab == tab)
            .await
            .with_context(|| format!("the Zellij tab {tab} shows no pane"))?;
        self.opened(session, pane.id).await
    }

    /// Open `shell` in a pane splitting `beside`, in its tab, which Zellij
    /// can list only after it answers.
    pub async fn open_split(&self, beside: PaneRef<'_>, shell: &Shell<'_>) -> Result<Opened> {
        let pane = run(self.new_split(beside, shell)?)
            .await
            .context("open a Zellij pane")?;
        let pane = pane.trim();
        self.new_pane(beside.session, |listed| listed.id == pane)
            .await
            .with_context(|| format!("the Zellij pane {pane} does not show"))?;
        self.opened(beside.session, pane.to_owned()).await
    }

    /// Open `shell` as the first pane of a new background session, named
    /// after its label and none other, exited ones included, in a tab that
    /// shows the label.
    pub async fn open_session(&self, shell: &Shell<'_>) -> Result<Opened> {
        let taken = self.session_names().await?;
        let session = free_session_name(shell.label, self.name_room(), &taken);
        run(self.new_session(&session, shell))
            .await
            .with_context(|| format!("create Zellij session {session}"))?;
        let pane = self
            .new_pane(&session, Pane::is_terminal)
            .await
            .with_context(|| format!("the Zellij session {session} shows no pane"))?;
        self.rename_tab(&session, &pane, shell.label).await?;
        self.opened(&session, pane.id).await
    }

    /// The first pane of `session` that is `new`, which Shoal just opened,
    /// once Zellij lists it.
    async fn new_pane(&self, session: &str, new: impl Fn(&Pane) -> bool) -> Result<Pane> {
        for _ in 0..PANE_POLLS {
            let panes = self.panes(session).await?;
            if let Some(pane) = panes.into_iter().find(&new) {
                return Ok(pane);
            }
            tokio::time::sleep(PANE_POLL).await;
        }
        bail!("Zellij lists no new pane")
    }

    /// The command that opens `shell` in a new tab of `session`.
    fn new_tab(&self, session: &str, shell: &Shell<'_>) -> Command {
        let mut open = self.action(session);
        open.args(["new-tab", "--name"])
            .arg(shell.label)
            .arg("--cwd")
            .arg(shell.cwd);
        if !shell.focus {
            open.arg("--no-focus");
        }
        open.args(pane_command(shell));
        open
    }

    /// The command that opens `shell` in a pane splitting `beside`, which
    /// Zellij takes as the pane the command runs in.
    fn new_split(&self, beside: PaneRef<'_>, shell: &Shell<'_>) -> Result<Command> {
        let mut open = self.action(beside.session);
        open.env(PANE_VAR, pane_number(beside.pane)?)
            .args(["new-pane", "--near-current-pane", "--direction", "right"])
            .arg("--name")
            .arg(shell.label)
            .arg("--cwd")
            .arg(shell.cwd);
        if !shell.focus {
            open.arg("--no-focus");
        }
        open.args(pane_command(shell));
        Ok(open)
    }

    /// The command that creates `session` in the background with `shell` as
    /// its first pane. Shoal's variables stay out of the session, whose every
    /// pane inherits its environment.
    fn new_session(&self, session: &str, shell: &Shell<'_>) -> Command {
        let mut create = self.command();
        create
            .args(["attach", "--create-background", session])
            .args(pane_command(shell))
            .current_dir(shell.cwd);
        for (variable, _) in std::env::vars_os() {
            if variable.to_string_lossy().starts_with(env::PREFIX) {
                create.env_remove(variable);
            }
        }
        create
    }

    /// How long a session's name may be: [`MAX_SESSION_LEN`], or less in a
    /// socket directory whose path leaves less room in a socket path.
    fn name_room(&self) -> usize {
        let used = self
            .socket_dir
            .as_deref()
            .map_or(0, |dir| dir.as_os_str().len() + CONTRACT_DIR.len());
        MAX_SOCKET_PATH
            .saturating_sub(used)
            .clamp(1, MAX_SESSION_LEN)
    }

    /// `pane` of `session`, which Shoal just opened, with its server.
    async fn opened(&self, session: &str, pane: String) -> Result<Opened> {
        let (instance, socket_dir) = self
            .instance(session)
            .await?
            .with_context(|| format!("no Zellij server runs session {session}"))?;
        Ok(Opened {
            session: session.to_owned(),
            pane,
            instance,
            socket_dir,
        })
    }

    /// Type `text` into `place` exactly as given, control characters
    /// included, while its server is the instance `instance`.
    pub async fn write(&self, place: PaneRef<'_>, instance: &str, text: &str) -> Result<()> {
        self.require(place, instance).await?;
        let mut write = self.action(place.session);
        write
            .args(["write", "--pane-id", place.pane])
            .args(text.bytes().map(|byte| byte.to_string()));
        run(write)
            .await
            .with_context(|| format!("type into Zellij pane {}", place.pane))?;
        Ok(())
    }

    /// The last `lines` lines `place` shows, its scrollback included, as
    /// text, while its server is the instance `instance`.
    pub async fn capture(
        &self,
        place: PaneRef<'_>,
        instance: &str,
        lines: usize,
    ) -> Result<String> {
        self.require(place, instance).await?;
        let mut dump = self.action(place.session);
        dump.args(["dump-screen", "--full", "--pane-id", place.pane]);
        let screen = run(dump)
            .await
            .with_context(|| format!("read Zellij pane {}", place.pane))?;
        Ok(last_lines(&screen, lines))
    }

    /// Make `pane` the focused pane of `session`, in its tab, which becomes
    /// the session's current one.
    pub async fn select(&self, session: &str, pane: &Pane) -> Result<()> {
        let mut tab = self.action(session);
        tab.args(["go-to-tab-by-id", &pane.tab.to_string()]);
        run(tab).await?;
        let mut focus = self.action(session);
        focus.args(["focus-pane-id", &pane.id]);
        run(focus).await?;
        Ok(())
    }

    /// Show `session`, at `pane`, in the Zellij client this process runs
    /// under, in the session `from`.
    pub async fn switch_client(&self, from: &str, session: &str, pane: &str) -> Result<()> {
        let mut switch = self.action(from);
        switch.args(["switch-session", session, "--pane-id", pane]);
        run(switch).await?;
        Ok(())
    }

    /// Attach this terminal to `session` until the user detaches.
    pub async fn attach(&self, session: &str) -> Result<()> {
        let mut attach = self.command();
        attach
            .args(["attach", session])
            .stdin(std::process::Stdio::inherit());
        let status = attach.status().await.context("run zellij")?;
        crate::execution::reset_input_modes();
        ensure!(status.success(), "zellij exited with {status}");
        Ok(())
    }

    /// Name the tab of `pane` `name`.
    pub async fn rename_tab(&self, session: &str, pane: &Pane, name: &str) -> Result<()> {
        let mut rename = self.action(session);
        rename.args(["rename-tab-by-id", &pane.tab.to_string(), name]);
        run(rename)
            .await
            .with_context(|| format!("rename Zellij tab {}", pane.tab))?;
        Ok(())
    }

    /// Close `place` on the server instance `instance` and nothing else, so
    /// the user's other panes and tabs stay. A session left with no pane
    /// ends, as Zellij ends a session whose last pane a client saw close. A
    /// pane already gone, or under its ID on a later instance, is closed;
    /// without `instance` nothing tells the pane apart from a later one, so
    /// it stays.
    pub async fn close(&self, place: PaneRef<'_>, instance: Option<&str>) -> Result<()> {
        let Some(instance) = instance else {
            return Ok(());
        };
        if !self.on_instance(place.session, instance).await? {
            return Ok(());
        }
        let panes = self.panes_on(place.session, Some(instance)).await?;
        // The session ends instead of closing its last tab: a client shows
        // the session end with that tab, and a `zellij action` whose server
        // ends before answering never returns.
        if panes.iter().all(|pane| pane.id == place.pane) {
            return self.end_session(place.session, instance).await;
        }
        if let Some(pane) = panes.iter().find(|pane| pane.id == place.pane) {
            self.close_pane(place.session, pane, &panes).await?;
        }
        Ok(())
    }

    /// Close `pane` of `session`, whose panes are `panes`, with its tab
    /// when no other pane is there: Zellij leaves a tab whose last pane
    /// closed while no client showed the session.
    async fn close_pane(&self, session: &str, pane: &Pane, panes: &[Pane]) -> Result<()> {
        let mut close = self.action(session);
        if alone_in_tab(pane, panes) {
            close.args(["close-tab-by-id", &pane.tab.to_string()]);
        } else {
            close.args(["close-pane", "--pane-id", &pane.id]);
        }
        run(close)
            .await
            .with_context(|| format!("close Zellij pane {}", pane.id))?;
        Ok(())
    }

    /// End `session`, which holds no pane, unless it ended already, asking
    /// again while the server instance `instance` still runs.
    async fn end_session(&self, session: &str, instance: &str) -> Result<()> {
        asked_while_running(
            || self.kill_session(session),
            || self.on_instance(session, instance),
        )
        .await
        .with_context(|| format!("end Zellij session {session}"))?;
        Ok(())
    }

    /// Ask Zellij to end `session`.
    async fn kill_session(&self, session: &str) -> Result<Reply<()>> {
        let mut kill = self.command();
        kill.args(["kill-session", session]);
        let output = Run::new(kill).timeout(TIMEOUT).capture().await?;
        killed(&output).with_context(|| format!("end Zellij session {session}"))
    }
}

/// What Zellij answered a command on one session.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reply<T> {
    /// The session's server answered; for `kill-session`, also when no
    /// session runs at all.
    Answered(T),
    /// Zellij found no answering server of that name.
    Missing,
}

impl<T> Reply<T> {
    /// What the server answered, if it did.
    fn answer(self) -> Option<T> {
        match self {
            Self::Answered(answer) => Some(answer),
            Self::Missing => None,
        }
    }
}

/// What `ask` answers once Zellij finds the session, or `None` once
/// `running` says the session's server no longer runs. Zellij counts a
/// session only when its server answers a probe in time, so it can report a
/// busy one as missing: Shoal asks again while the server still runs.
async fn asked_while_running<T, Asked, Running>(
    mut ask: impl FnMut() -> Asked,
    mut running: impl FnMut() -> Running,
) -> Result<Option<T>>
where
    Asked: Future<Output = Result<Reply<T>>>,
    Running: Future<Output = Result<bool>>,
{
    for _ in 0..MISSING_ATTEMPTS {
        if let Reply::Answered(answer) = ask().await? {
            return Ok(Some(answer));
        }
        if !running().await? {
            return Ok(None);
        }
        tokio::time::sleep(MISSING_RETRY).await;
    }
    bail!("Zellij reported the running session as missing")
}

/// The arguments that run `shell`'s command in a new pane, which closes
/// once it ends; none for the default shell.
fn pane_command<'a>(shell: &Shell<'a>) -> Vec<&'a str> {
    if shell.command.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["--close-on-exit", "--"];
    args.extend(shell.command.iter().map(String::as_str));
    args
}

/// What `command` prints, failing when it fails.
async fn run(command: Command) -> Result<String> {
    let output = Run::new(command).timeout(TIMEOUT).checked().await?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What the listing `command` prints, or that Zellij found the session, or
/// any session, missing.
async fn listing(command: Command) -> Result<Reply<String>> {
    listed(&Run::new(command).timeout(TIMEOUT).capture().await?)
}

/// What a listing command that ended with `output` printed; see [`listing`].
/// A failure names Zellij's message, which some commands print to stdout.
fn listed(output: &Output) -> Result<Reply<String>> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        return Ok(Reply::Answered(stdout.into_owned()));
    }
    let error = crate::subprocess::diagnostic(&output.stderr);
    if nothing_to_list(&error) || nothing_to_list(&stdout) {
        return Ok(Reply::Missing);
    }
    let reason = if error.trim().is_empty() {
        crate::subprocess::diagnostic(&output.stdout)
    } else {
        error
    };
    bail!("zellij failed: {}", reason.trim())
}

/// The place ID of `pane` in `session`.
pub(crate) fn place_id(session: &str, pane: &str) -> String {
    format!("{session}:{pane}")
}

/// The session and pane a place ID names; the session may hold `:`, the
/// pane never does.
pub(crate) fn parse_place(id: &str) -> Option<PaneRef<'_>> {
    let (session, pane) = id.rsplit_once(':')?;
    (!session.is_empty() && pane.starts_with("terminal_")).then_some(PaneRef { session, pane })
}

/// The pane `ZELLIJ_PANE_ID` names by its number.
pub(crate) fn pane_of_number(number: &str) -> Option<String> {
    number
        .parse::<u64>()
        .ok()
        .map(|number| format!("terminal_{number}"))
}

/// The number of `pane`, as `ZELLIJ_PANE_ID` names it.
fn pane_number(pane: &str) -> Result<&str> {
    pane.strip_prefix("terminal_")
        .filter(|number| number.parse::<u64>().is_ok())
        .with_context(|| format!("invalid Zellij pane {pane}"))
}

/// The value of variable `name`, when set and not empty.
pub(crate) fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Whether `a` and `b` name the same directory.
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .ok()
            .zip(std::fs::canonicalize(b).ok())
            .is_some_and(|(a, b)| a == b)
}

/// A `ps` line of `uid`'s Zellij server for `session`: `zellij --server
/// <socket dir>/<contract>/<session>`.
fn parse_server(line: &str, uid: u32, session: &str) -> Option<Server> {
    let mut fields = line.split_whitespace();
    let owner: u32 = fields.next()?.parse().ok()?;
    let pid: u32 = fields.next()?.parse().ok()?;
    let program = fields.next()?;
    if owner != uid || Path::new(program).file_name()? != "zellij" {
        return None;
    }
    let (_, socket) = line.split_once(" --server ")?;
    let socket = Path::new(socket.trim_end());
    if socket.file_name()?.to_str()? != session {
        return None;
    }
    Some(Server {
        pid,
        socket_dir: socket.parent()?.parent()?.to_owned(),
    })
}

/// The terminals and the user's plugins `list-panes --json` lists, without
/// the plugins Zellij shows in every tab or keeps hidden.
fn parse_panes(listed: &str) -> Result<Vec<Pane>> {
    if listed.trim().is_empty() {
        return Ok(Vec::new());
    }
    let listed: Vec<Listed> = serde_json::from_str(listed).context("invalid Zellij panes")?;
    Ok(listed
        .into_iter()
        .filter(|pane| !pane.is_plugin || !(pane.is_suppressed || in_layout(pane)))
        .map(|pane| Pane {
            id: format!(
                "{}_{}",
                if pane.is_plugin { "plugin" } else { "terminal" },
                pane.id
            ),
            tab: pane.tab_id,
            tab_name: pane.tab_name,
            command: pane.pane_command,
            exited: pane.exited,
        })
        .collect())
}

/// Whether `pane` is the only one of `panes` in its tab.
fn alone_in_tab(pane: &Pane, panes: &[Pane]) -> bool {
    panes
        .iter()
        .all(|other| other.id == pane.id || other.tab != pane.tab)
}

/// Whether the plugin `pane` is one Zellij's layouts put in every tab.
fn in_layout(pane: &Listed) -> bool {
    pane.plugin_url
        .as_deref()
        .is_some_and(|url| LAYOUT_PLUGINS.contains(&url.strip_prefix("zellij:").unwrap_or(url)))
}

/// The pane each listed client shows, after the header.
fn parse_clients(listed: &str) -> Vec<String> {
    listed
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(str::to_owned)
        .collect()
}

/// The names `list-sessions --no-formatting` lists, before their creation
/// time.
fn parse_sessions(listed: &str) -> Vec<String> {
    listed
        .lines()
        .filter_map(|line| line.rsplit_once(" [Created "))
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// A session name for `label` that none of `taken` has: what Zellij keeps,
/// without `/`, up to `room` bytes for its socket path with the `-2`, `-3`
/// it takes while taken.
fn free_session_name(label: &str, room: usize, taken: &[String]) -> String {
    let label = label.replace('/', "_");
    let named = |n: u32| {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let mut end = room.saturating_sub(suffix.len()).min(label.len());
        while !label.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}{suffix}", label[..end].trim_end())
    };
    (1..)
        .map(named)
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| named(1))
}

/// The last `lines` lines of `screen`, without the blank lines below what
/// it shows.
fn last_lines(screen: &str, lines: usize) -> String {
    let shown: Vec<&str> = screen.trim_end().lines().collect();
    let start = shown.len().saturating_sub(lines);
    let mut text = shown[start..].join("\n");
    text.push('\n');
    text
}

/// The major and minor version `zellij --version` reports, such as 0.45
/// for `zellij 0.45.1`.
fn parse_version(output: &str) -> Option<(u32, u32)> {
    let version = output.trim().strip_prefix("zellij ")?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((major, minor.parse().ok()?))
}

/// Whether Zellij's `message` says it found the session, or any session,
/// missing.
fn nothing_to_list(message: &str) -> bool {
    message.contains("There is no active session")
        || message.contains("No active zellij sessions found")
        || message.contains("' not found")
}

/// What a `zellij kill-session` that ended with `output` did. Zellij says
/// on stdout when no session of the name answered among the others; when it
/// finds no session at all, nothing is left to end.
fn killed(output: &Output) -> Result<Reply<()>> {
    if !output.status.success()
        && String::from_utf8_lossy(&output.stdout).contains("No session named ")
    {
        return Ok(Reply::Missing);
    }
    listed(output)?;
    Ok(Reply::Answered(()))
}

/// Zellij panes carry no Shoal labels, so the daemon's records of the panes
/// Shoal created are the proof of ownership: discovery finds none, and a
/// close needs a recorded pane, which is Shoal's while it is open on the
/// server instance the record names.
pub(super) struct Cleanup;

impl super::Host for Cleanup {
    fn name(&self) -> super::HostName {
        super::HostName::Zellij
    }

    fn ensure_installed(&self) -> Result<()> {
        ensure!(
            Tool::Zellij.installed(),
            "zellij is not installed or not on PATH"
        );
        Ok(())
    }

    fn server(&self, data: &BTreeMap<String, String>) -> String {
        data.get(SOCKET_DIR).cloned().unwrap_or_default()
    }

    fn sessions<'a>(&'a self, _state: &'a Path) -> BoxFuture<'a, Result<Vec<super::Session>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn close<'a>(
        &'a self,
        _state: &'a Path,
        place: &'a super::Origin,
        _owner: &'a super::Owner,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            ensure!(
                place.owned,
                "Shoal did not create Zellij pane {}",
                place.id()
            );
            let pane = parse_place(place.id())
                .with_context(|| format!("invalid Zellij place {}", place.id()))?;
            Zellij::recorded(&place.data)
                .reaching(pane.session)
                .await?
                .close(pane, place.instance.as_deref())
                .await
        })
    }
}

#[cfg(test)]
mod tests;
