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
/// What a Zellij origin's data records for a session Shoal created for the
/// place alone.
const OWN_SESSION: &str = "own_session";
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
/// How often, and how many times, Shoal reads a new tab's or session's
/// panes until its pane shows; only a hang guard, since Zellij lists it
/// soon after it answers.
const PANE_POLL: Duration = Duration::from_millis(100);
const PANE_POLLS: u32 = 100;

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

/// A pane and where it sits now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pane {
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
    /// Whether Shoal created the session for this pane alone.
    pub own_session: bool,
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
    tab_id: u64,
    #[serde(default)]
    tab_name: String,
    #[serde(default)]
    exited: bool,
    #[serde(default)]
    pane_command: Option<String>,
}

impl Zellij {
    /// The sessions of the socket directory `data` records, otherwise the
    /// ones this process reaches.
    pub fn recorded(data: &BTreeMap<String, String>) -> Self {
        match data.get(SOCKET_DIR) {
            Some(dir) => Self {
                socket_dir: Some(PathBuf::from(dir)),
            },
            None => Self::here(),
        }
    }

    /// The sessions this process reaches.
    pub fn here() -> Self {
        Self {
            socket_dir: var(SOCKET_DIR_VAR).map(PathBuf::from),
        }
    }

    /// What an origin records to reach a session in `socket_dir`, and
    /// whether Shoal created that session for the place alone.
    pub fn data(socket_dir: &Path, own_session: bool) -> BTreeMap<String, String> {
        let mut data = BTreeMap::from([(
            SOCKET_DIR.to_owned(),
            socket_dir.to_string_lossy().into_owned(),
        )]);
        if own_session {
            data.insert(OWN_SESSION.to_owned(), "1".to_owned());
        }
        data
    }

    /// The socket directory these sessions listen in, where the caller or
    /// a record chose one.
    pub fn socket_dir(&self) -> Option<&Path> {
        self.socket_dir.as_deref()
    }

    /// Whether `data` records a session Shoal created for its place alone.
    pub fn own_session(data: &BTreeMap<String, String>) -> bool {
        data.contains_key(OWN_SESSION)
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
        let mut list = self.action(session);
        list.args(["list-panes", "--all", "--json"]);
        let listed = listing(list).await?;
        parse_panes(&listed)
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
            .panes(place.session)
            .await?
            .into_iter()
            .find(|pane| pane.id == place.pane))
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
        let mut list = self.action(session);
        list.arg("list-clients");
        Ok(parse_clients(&listing(list).await?))
    }

    /// Every session's name, exited ones included, which Zellij would
    /// resurrect rather than create under that name.
    async fn session_names(&self) -> Result<Vec<String>> {
        let mut list = self.command();
        list.args(["list-sessions", "--no-formatting"]);
        Ok(parse_sessions(&listing(list).await?))
    }

    /// Open `shell` in a new tab of `session`, which a client shows: Zellij
    /// drops tabs opened in a session no client shows.
    pub async fn open_tab(&self, session: &str, shell: &Shell<'_>) -> Result<Opened> {
        let mut open = self.action(session);
        open.args(["new-tab", "--name"])
            .arg(shell.label)
            .arg("--cwd")
            .arg(shell.cwd);
        if !shell.focus {
            open.arg("--no-focus");
        }
        if !shell.command.is_empty() {
            open.arg("--close-on-exit").arg("--").args(shell.command);
        }
        let tab = run(open).await.context("open a Zellij tab")?;
        let tab: u64 = tab.trim().parse().context("invalid Zellij tab")?;
        let pane = self
            .new_pane(session, |pane| pane.tab == tab)
            .await
            .with_context(|| format!("the Zellij tab {tab} shows no pane"))?;
        self.opened(session, pane.id, false).await
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

    /// Open `shell` in a pane splitting `beside`, in its tab.
    pub async fn open_split(&self, beside: PaneRef<'_>, shell: &Shell<'_>) -> Result<Opened> {
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
        if !shell.command.is_empty() {
            open.arg("--close-on-exit").arg("--").args(shell.command);
        }
        let pane = run(open).await.context("open a Zellij pane")?;
        self.opened(beside.session, pane.trim().to_owned(), false)
            .await
    }

    /// Open `shell` as the first pane of a new background session, named
    /// after its label and none other, exited ones included, in a tab that
    /// shows the label. Shoal's variables stay out of the session, whose
    /// every pane inherits its environment.
    pub async fn open_session(&self, shell: &Shell<'_>) -> Result<Opened> {
        let room = self.name_room();
        let session = free_session_name(shell.label, room, &self.session_names().await?);
        let mut create = self.command();
        create
            .args(["attach", "--create-background"])
            .arg(&session)
            .current_dir(shell.cwd);
        if !shell.command.is_empty() {
            create.arg("--close-on-exit").arg("--").args(shell.command);
        }
        for (variable, _) in std::env::vars_os() {
            if variable.to_string_lossy().starts_with(env::PREFIX) {
                create.env_remove(variable);
            }
        }
        run(create)
            .await
            .with_context(|| format!("create Zellij session {session}"))?;
        let pane = self
            .new_pane(&session, |_| true)
            .await
            .with_context(|| format!("the Zellij session {session} shows no pane"))?;
        self.rename_tab(&session, &pane, shell.label).await?;
        self.opened(&session, pane.id, true).await
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

    /// `pane` of `session`, which Shoal just opened, with its server, in a
    /// session Shoal created for it when `own_session`.
    async fn opened(&self, session: &str, pane: String, own_session: bool) -> Result<Opened> {
        let (instance, socket_dir) = self
            .instance(session)
            .await?
            .with_context(|| format!("no Zellij server runs session {session}"))?;
        Ok(Opened {
            session: session.to_owned(),
            pane,
            instance,
            socket_dir,
            own_session,
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
    /// the user's other panes and tabs stay; a session Shoal created for the
    /// place alone ends with it while no other pane is there. A pane
    /// already gone, or under its ID on a later instance, is closed; without
    /// `instance` nothing tells the pane apart from a later one, so it stays.
    pub async fn close(
        &self,
        place: PaneRef<'_>,
        instance: Option<&str>,
        own_session: bool,
    ) -> Result<()> {
        let Some(instance) = instance else {
            return Ok(());
        };
        if !self.on_instance(place.session, instance).await? {
            return Ok(());
        }
        let panes = self.panes(place.session).await?;
        let others = panes.iter().any(|pane| pane.id != place.pane);
        if own_session && !others {
            return self.end_session(place.session).await;
        }
        match panes.iter().find(|pane| pane.id == place.pane) {
            Some(pane) => self.close_pane(place.session, pane, &panes).await,
            None => Ok(()),
        }
    }

    /// Close `pane` of `session`, whose panes are `panes`, with its tab
    /// when no other pane is there: Zellij leaves a tab whose last pane
    /// closed while no client showed the session.
    async fn close_pane(&self, session: &str, pane: &Pane, panes: &[Pane]) -> Result<()> {
        let alone = panes
            .iter()
            .all(|other| other.id == pane.id || other.tab != pane.tab);
        let mut close = self.action(session);
        if alone {
            close.args(["close-tab-by-id", &pane.tab.to_string()]);
        } else {
            close.args(["close-pane", "--pane-id", &pane.id]);
        }
        run(close)
            .await
            .with_context(|| format!("close Zellij pane {}", pane.id))?;
        Ok(())
    }

    /// End `session` and forget it, so Zellij offers no exited session to
    /// resurrect.
    async fn end_session(&self, session: &str) -> Result<()> {
        let mut kill = self.command();
        kill.args(["kill-session", session]);
        run(kill)
            .await
            .with_context(|| format!("end Zellij session {session}"))?;
        let mut delete = self.command();
        delete.args(["delete-session", session]);
        let _ = Run::new(delete).capture().await;
        Ok(())
    }
}

/// What `command` prints, failing when it fails.
async fn run(command: Command) -> Result<String> {
    let output = Run::new(command).checked().await?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What `command` prints, a listing, which is empty when the session or
/// every session is gone.
async fn listing(command: Command) -> Result<String> {
    let output = Run::new(command).capture().await?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let error = crate::subprocess::diagnostic(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if nothing_to_list(&error) || nothing_to_list(&stdout) {
        return Ok(String::new());
    }
    bail!("zellij failed: {error}")
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

fn parse_panes(listed: &str) -> Result<Vec<Pane>> {
    if listed.trim().is_empty() {
        return Ok(Vec::new());
    }
    let listed: Vec<Listed> = serde_json::from_str(listed).context("invalid Zellij panes")?;
    Ok(listed
        .into_iter()
        .filter(|pane| !pane.is_plugin)
        .map(|pane| Pane {
            id: format!("terminal_{}", pane.id),
            tab: pane.tab_id,
            tab_name: pane.tab_name,
            command: pane.pane_command,
            exited: pane.exited,
        })
        .collect())
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
/// without `/`, up to `room` bytes for its socket path, suffixed while taken.
fn free_session_name(label: &str, room: usize, taken: &[String]) -> String {
    let mut base: String = label.replace('/', "_");
    if base.len() > room {
        let mut end = room;
        while !base.is_char_boundary(end) {
            end -= 1;
        }
        base.truncate(end);
    }
    let base = base.trim_end().to_owned();
    (1..)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|name| !taken.contains(name))
        .unwrap_or(base)
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

/// Whether Zellij's `message` says the session, or every session, is gone.
fn nothing_to_list(message: &str) -> bool {
    message.contains("There is no active session")
        || message.contains("No active zellij sessions found")
        || message.contains("' not found")
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
                .close(
                    pane,
                    place.instance.as_deref(),
                    Zellij::own_session(&place.data),
                )
                .await
        })
    }
}

#[cfg(test)]
mod tests;
