//! The tmux CLI against one tmux server, for the terminal host and the
//! daemon's cleanup alike. tmux numbers sessions, windows and panes afresh
//! each time its server starts, so a place records the server's instance, its
//! process and start time, beside the pane ID: a pane under a recorded ID is
//! the recorded one only on the server instance that created it.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use anyhow::{Context as _, Result, bail, ensure};
use futures_util::future::BoxFuture;
use tokio::process::Command;

use crate::{env, subprocess::Run, tools::Tool};

/// What a tmux origin's data records: the server's socket.
const SOCKET: &str = "socket";
/// The oldest tmux Shoal supports, the first that can pass terminal protocols
/// through to the outer terminal (`allow-passthrough`).
const OLDEST: (u32, u32) = (3, 3);
/// What tmux reports for each pane Shoal reads, space-separated since newer
/// tmux prints tabs as `_`, with the window name last as the one field that
/// may hold a space.
const PANE_FORMAT: &str = "#{pane_id} #{window_id} #{window_active} #{session_attached} \
    #{pane_in_mode} #{pane_pid} #{pid}:#{start_time} #{window_name}";
/// What tmux reports for a session: its ID and name.
const SESSION_FORMAT: &str = "#{session_id} #{session_name}";
/// A server instance, its process and start time, as tmux formats it.
const INSTANCE_FORMAT: &str = "#{pid}:#{start_time}";
/// What tmux reports for a pane it opened, the socket path last.
const OPENED_FORMAT: &str = "#{pane_id} #{pid}:#{start_time} #{socket_path}";

/// The variable naming the directory of the default server's socket.
pub(crate) const SOCKET_DIR_VAR: &str = "TMUX_TMPDIR";
/// The variables that choose the server `tmux` reaches: the one a pane runs
/// on, and the directory of the default server's socket.
pub(crate) const SERVER_VARS: &[&str] = &["TMUX", SOCKET_DIR_VAR];

/// The socket `data` records, empty for the default server.
fn server(data: &BTreeMap<String, String>) -> String {
    data.get(SOCKET).cloned().unwrap_or_default()
}

/// A tmux server, by its socket; `None` is the server `tmux` reaches by
/// default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tmux {
    socket: Option<PathBuf>,
}

/// A pane and where it sits now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pane {
    pub id: String,
    pub window: String,
    pub window_name: String,
    /// Clients showing it: those attached to its session, while its window
    /// is the session's current one.
    pub clients: u32,
    /// Whether it shows a mode, such as copy mode, over its program.
    pub in_mode: bool,
    /// The process tmux started in it, usually a shell.
    pub pid: u32,
    /// The server instance it belongs to.
    pub instance: String,
}

/// A session, with the socket of the server it runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Group {
    pub session: String,
    pub socket: String,
}

/// A pane Shoal opened, with the server instance and socket it opened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opened {
    pub pane: String,
    pub instance: String,
    pub socket: String,
}

/// A pane to open in a new window of a session, or splitting a pane.
pub(crate) struct Shell<'a> {
    pub session: &'a str,
    pub label: &'a str,
    pub cwd: &'a Path,
    pub focus: bool,
    /// What the pane runs, without a shell; empty runs the default shell.
    pub command: &'a [String],
    /// The pane to split for it; `None` opens a window of its own.
    pub beside: Option<&'a str>,
}

impl Tmux {
    /// The server `data` records.
    pub fn recorded(data: &BTreeMap<String, String>) -> Self {
        Self {
            socket: data.get(SOCKET).map(PathBuf::from),
        }
    }

    /// The server this process reaches: the one its pane runs on, otherwise
    /// the default server.
    pub fn here() -> Self {
        Self {
            socket: var("TMUX").and_then(|tmux| parse_tmux_var(&tmux).map(|var| var.socket.into())),
        }
    }

    /// What an origin records to reach the server listening on `socket`.
    pub fn data(socket: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(SOCKET.to_owned(), socket.to_owned())])
    }

    /// A `tmux` command for this server. Empty variables, as a start queued
    /// from no terminal carries, are unset, which tmux does not read them as.
    fn command(&self) -> Command {
        let mut command = Command::new(Tool::Tmux.program());
        command.stdin(std::process::Stdio::null());
        if let Some(socket) = &self.socket {
            command.arg("-S").arg(socket);
        }
        for name in SERVER_VARS {
            if std::env::var_os(name).is_some_and(|value| value.is_empty()) {
                command.env_remove(name);
            }
        }
        command
    }

    async fn run<S: AsRef<std::ffi::OsStr>>(&self, args: &[S]) -> Result<String> {
        let mut command = self.command();
        command.args(args);
        let output = Run::new(command).checked().await?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Refuse a `tmux` older than [`OLDEST`].
    pub async fn ensure_supported(&self) -> Result<()> {
        let version = self.run(&["-V"]).await?;
        if let Some(found) = parse_version(&version) {
            ensure!(
                found >= OLDEST,
                "{} is too old; Shoal needs tmux {}.{} or later",
                version.trim(),
                OLDEST.0,
                OLDEST.1
            );
        }
        Ok(())
    }

    /// What `tmux` prints for `args`, a listing, which is empty when no
    /// server listens or it has no sessions.
    async fn list(&self, args: &[&str]) -> Result<String> {
        let mut list = self.command();
        list.args(args);
        let output = Run::new(list).capture().await?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let error = crate::subprocess::diagnostic(&output.stderr);
        if nothing_to_list(&error) {
            return Ok(String::new());
        }
        bail!("tmux {} failed: {error}", args.join(" "))
    }

    /// Every pane on the server; none when no server listens.
    pub async fn panes(&self) -> Result<Vec<Pane>> {
        let listed = self.list(&["list-panes", "-a", "-F", PANE_FORMAT]).await?;
        Ok(listed.lines().filter_map(parse_pane).collect())
    }

    /// The pane `id`, while it is open on the server instance `instance`,
    /// or on any instance without one.
    pub async fn pane(&self, id: &str, instance: Option<&str>) -> Result<Option<Pane>> {
        Ok(self.panes().await?.into_iter().find(|pane| {
            pane.id == id && instance.is_none_or(|instance| pane.instance == instance)
        }))
    }

    /// The first session named `name`, created at `cwd` when there is none.
    pub async fn session(&self, name: &str, cwd: &Path) -> Result<Group> {
        let name = session_name(name);
        let session = match self.find_session(&name).await? {
            Some(session) => session,
            None => self.create_session(&name, cwd).await?,
        };
        self.group(session).await
    }

    /// The ID of the first session named `name`.
    async fn find_session(&self, name: &str) -> Result<Option<String>> {
        let listed = self.list(&["list-sessions", "-F", SESSION_FORMAT]).await?;
        Ok(listed
            .lines()
            .filter_map(parse_session)
            .find_map(|(session, listed)| (listed == name).then(|| session.to_owned())))
    }

    /// A new session named `name` at `cwd`, which starts the server when none
    /// listens. Shoal's variables stay out of a server this starts, whose
    /// environment every later pane on it inherits.
    async fn create_session(&self, name: &str, cwd: &Path) -> Result<String> {
        let mut create = self.command();
        create
            .args(["new-session", "-d", "-P", "-F", "#{session_id}", "-s"])
            .arg(literal(name))
            .arg("-c")
            .arg(literal(cwd));
        for (variable, _) in std::env::vars_os() {
            if variable.to_string_lossy().starts_with(env::PREFIX) {
                create.env_remove(variable);
            }
        }
        let output = Run::new(create)
            .checked()
            .await
            .with_context(|| format!("create tmux session {name}"))?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned())
    }

    /// The session `session` and the socket of the server it runs on.
    async fn group(&self, session: String) -> Result<Group> {
        let socket = self
            .run(&["display-message", "-p", "-t", &session, "#{socket_path}"])
            .await?;
        Ok(Group {
            socket: socket.trim_end_matches('\n').to_owned(),
            session,
        })
    }

    /// Open `shell` in a window of its own, or split beside the pane it
    /// names.
    pub async fn open(&self, shell: &Shell<'_>) -> Result<Opened> {
        let output = self
            .run(&open_args(shell))
            .await
            .context("open a tmux window")?;
        parse_opened(&output).context("invalid tmux pane")
    }

    /// Type `text` into `pane` exactly as given, control characters
    /// included, through a paste buffer of its own: tmux reads arguments
    /// that end in `;` as separators, while a buffer passes everything.
    /// Only while the server is the instance `instance`.
    pub async fn send(&self, pane: &str, instance: &str, text: &str) -> Result<()> {
        static SENT: AtomicU32 = AtomicU32::new(0);
        let buffer = format!(
            "shoal-{}-{}",
            std::process::id(),
            SENT.fetch_add(1, Ordering::Relaxed)
        );
        let paste = words(&["paste-buffer", "-d", "-r", "-b", &buffer, "-t", pane]);
        let discard = words(&["delete-buffer", "-b", &buffer]);
        let mut load = self.command();
        load.args(["load-buffer", "-b", &buffer, "-", ";"])
            .args(on_instance(pane, instance, &[paste], &[discard])?);
        Run::new(load)
            .input(text.as_bytes())
            .checked()
            .await
            .with_context(|| format!("type into tmux pane {pane}"))?;
        Ok(())
    }

    /// Run `command` in `pane` in place of what it runs, while the server
    /// is the instance `instance`, which tmux checks as it respawns it.
    pub async fn respawn(&self, pane: &str, instance: &str, command: &[String]) -> Result<()> {
        let mut respawn = vec![
            "respawn-pane".to_owned(),
            "-k".into(),
            "-t".into(),
            pane.into(),
        ];
        respawn.extend_from_slice(command);
        self.run(&on_instance(pane, instance, &[respawn], &[])?)
            .await
            .with_context(|| format!("run a command in tmux pane {pane}"))?;
        Ok(())
    }

    /// The last lines `pane` shows, its scrollback included, as text.
    /// Only while the server is the instance `instance`, and nothing
    /// otherwise.
    pub async fn capture(&self, pane: &str, instance: &str, lines: u32) -> Result<String> {
        let start = format!("-{lines}");
        let capture = words(&["capture-pane", "-p", "-J", "-S", &start, "-t", pane]);
        self.run(&on_instance(pane, instance, &[capture], &[])?)
            .await
            .with_context(|| format!("read tmux pane {pane}"))
    }

    /// Leave the mode `pane` shows, such as copy mode, so typing reaches its
    /// program, while the server is the instance that listed the pane.
    pub async fn leave_mode(&self, pane: &Pane) -> Result<()> {
        let cancel = words(&["send-keys", "-X", "-t", &pane.id, "cancel"]);
        self.run(&on_instance(&pane.id, &pane.instance, &[cancel], &[])?)
            .await?;
        Ok(())
    }

    /// Make `pane` the active pane of its window, and the window the current
    /// one of its session, while the server is the instance that listed the
    /// pane.
    pub async fn select(&self, pane: &Pane) -> Result<()> {
        let window = words(&["select-window", "-t", &pane.id]);
        let active = words(&["select-pane", "-t", &pane.id]);
        self.run(&on_instance(
            &pane.id,
            &pane.instance,
            &[window, active],
            &[],
        )?)
        .await?;
        Ok(())
    }

    /// Show `pane`'s session in the tmux client this process runs under.
    pub async fn switch_client(&self, pane: &str) -> Result<()> {
        self.run(&["switch-client", "-t", pane]).await?;
        Ok(())
    }

    /// Attach this terminal to `pane`'s session until the user detaches.
    pub async fn attach(&self, pane: &str) -> Result<()> {
        let mut attach = self.command();
        attach
            .args(["attach-session", "-t", pane])
            .stdin(std::process::Stdio::inherit());
        let status = attach.status().await.context("run tmux")?;
        crate::execution::reset_input_modes();
        ensure!(status.success(), "tmux exited with {status}");
        Ok(())
    }

    /// Name the window of `pane` `name`, which keeps tmux from renaming it
    /// after its program, while the server is the instance that listed the
    /// pane, the pane is alone in its window and, with `expected`, the
    /// window shows that name. tmux checks all of it as it renames, so a
    /// window that changed since the listing keeps its name.
    pub async fn rename_window(
        &self,
        pane: &Pane,
        expected: Option<&str>,
        name: &str,
    ) -> Result<()> {
        let rename = words(&["rename-window", "-t", &pane.id, &plain_name(name)]);
        let condition = rename_condition(&pane.instance, expected)?;
        self.run(&when(&pane.id, &condition, &[rename], &[]))
            .await
            .with_context(|| format!("rename tmux window {}", pane.window))?;
        Ok(())
    }

    /// Whether `pane`'s shell is back at its prompt: nothing runs in the
    /// foreground of its terminal but the process tmux started there.
    pub async fn at_prompt(&self, pane: &Pane) -> bool {
        let mut ps = Command::new(Tool::Ps.program());
        ps.args(["-o", "tpgid=", "-p", &pane.pid.to_string()]);
        Run::new(ps).checked().await.is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout).trim() == pane.pid.to_string()
        })
    }

    /// Close `pane` on the server instance `instance` and nothing else:
    /// tmux closes a window with its last pane and a session with its last
    /// window, so the user's other panes stay. A pane already gone, or under
    /// its ID on a later instance, is closed. The server checks the
    /// instance as it kills the pane, so a restart in between closes
    /// nothing; without `instance` nothing tells the pane apart from a later
    /// one, so it stays.
    pub async fn close(&self, pane: &str, instance: Option<&str>) -> Result<()> {
        let Some(instance) = instance else {
            return Ok(());
        };
        if self.pane(pane, Some(instance)).await?.is_none() {
            return Ok(());
        }
        let mut kill = self.command();
        let close = words(&["kill-pane", "-t", pane]);
        kill.args(on_instance(pane, instance, &[close], &[])?);
        let output = Run::new(kill).capture().await?;
        ensure!(
            output.status.success() || self.pane(pane, Some(instance)).await?.is_none(),
            "cannot close tmux pane {pane}: {}",
            crate::subprocess::diagnostic(&output.stderr)
        );
        Ok(())
    }
}

/// What `TMUX` holds in a tmux pane: the server's socket and process, and
/// the session the pane started in.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TmuxVar<'a> {
    pub socket: &'a str,
    pub session: String,
}

/// `TMUX`, `<socket>,<server pid>,<session number>`, as tmux sets it.
pub(crate) fn parse_tmux_var(value: &str) -> Option<TmuxVar<'_>> {
    let mut fields = value.rsplitn(3, ',');
    let session = fields.next()?;
    fields.next()?;
    let socket = fields.next().filter(|socket| !socket.is_empty())?;
    session.parse::<u32>().ok()?;
    Some(TmuxVar {
        socket,
        session: format!("${session}"),
    })
}

/// The value of variable `name`, when set and not empty.
pub(crate) fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The `tmux` arguments that open `shell` and print the pane they opened.
fn open_args(shell: &Shell<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = match shell.beside {
        Some(anchor) => vec![
            "split-window".into(),
            "-h".into(),
            "-t".into(),
            anchor.into(),
        ],
        None => vec![
            "new-window".into(),
            "-t".into(),
            format!("{}:", shell.session).into(),
            "-n".into(),
            literal(plain_name(shell.label)),
        ],
    };
    args.extend(["-P".into(), "-F".into(), OPENED_FORMAT.into(), "-c".into()]);
    args.push(literal(shell.cwd));
    if !shell.focus {
        args.push("-d".into());
    }
    args.extend(shell.command.iter().map(literal));
    args
}

/// `arg` as tmux reads it back from its command line, where an argument
/// ending in `;` ends the command unless the `;` is escaped.
fn literal(arg: impl AsRef<std::ffi::OsStr>) -> OsString {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let bytes = arg.as_ref().as_bytes();
    match bytes.strip_suffix(b";") {
        Some(start) => OsString::from_vec([start, b"\\;"].concat()),
        None => arg.as_ref().to_owned(),
    }
}

/// `commands` on `pane`, run in order only while the server is the
/// instance `instance`, which tmux checks as it runs them, so a pane ID the
/// server reused after a restart is left alone; `otherwise` runs instead.
fn on_instance(
    pane: &str,
    instance: &str,
    commands: &[Vec<String>],
    otherwise: &[Vec<String>],
) -> Result<Vec<String>> {
    Ok(when(pane, &is_instance(instance)?, commands, otherwise))
}

/// The tmux format that is true while the server is the instance
/// `instance`.
fn is_instance(instance: &str) -> Result<String> {
    ensure!(
        instance
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b':'),
        "invalid tmux server instance {instance}"
    );
    Ok(format!("#{{==:{INSTANCE_FORMAT},{instance}}}"))
}

/// The tmux format that is true while both formats are.
fn both(first: &str, second: &str) -> String {
    format!("#{{&&:{first},{second}}}")
}

/// `commands` on `pane`, run in order only while the tmux format
/// `condition` is true there, which tmux checks as it runs them;
/// `otherwise` runs instead.
fn when(
    pane: &str,
    condition: &str,
    commands: &[Vec<String>],
    otherwise: &[Vec<String>],
) -> Vec<String> {
    let mut args = vec![
        "if-shell".into(),
        "-F".into(),
        "-t".into(),
        pane.to_owned(),
        condition.to_owned(),
        command_list(commands),
    ];
    if !otherwise.is_empty() {
        args.push(command_list(otherwise));
    }
    args
}

/// `name` as a window name tmux keeps as it is: tmux expands formats in
/// the names it is given, where `#` starts one.
fn plain_name(name: &str) -> String {
    name.replace('#', "##")
}

/// `text` as an argument of a tmux format such as `#{==:a,b}`, which reads
/// it as itself.
fn format_argument(text: &str) -> String {
    text.replace('#', "##")
        .replace(',', "#,")
        .replace('}', "#}")
}

/// The tmux format, on a pane, that is true while the server is the
/// instance `instance`, the pane is alone in its window and, with
/// `expected`, the window shows that name.
fn rename_condition(instance: &str, expected: Option<&str>) -> Result<String> {
    let alone = both(&is_instance(instance)?, "#{==:#{window_panes},1}");
    Ok(match expected {
        Some(expected) => {
            let shows = format!("#{{==:#{{window_name}},{}}}", format_argument(expected));
            both(&alone, &shows)
        }
        None => alone,
    })
}

/// `commands` as one tmux command list, as `if-shell` takes it.
fn command_list(commands: &[Vec<String>]) -> String {
    commands
        .iter()
        .map(|command| crate::shell::quote(command))
        .collect::<Vec<_>>()
        .join(" ; ")
}

/// `words` as an owned command.
fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// `name` as tmux keeps a session name, which cannot hold `.` or `:`.
fn session_name(name: &str) -> String {
    name.replace(['.', ':'], "_")
}

/// The major and minor version `tmux -V` reports, such as 3.5 for `tmux
/// 3.5a` or `tmux next-3.6`; none for a build without one.
fn parse_version(output: &str) -> Option<(u32, u32)> {
    let version = output.trim().strip_prefix("tmux ")?;
    let version = version.strip_prefix("next-").unwrap_or(version);
    let (major, rest) = version.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Whether tmux's `error` says no server listens on the socket, or the
/// server has no session to list from. A socket tmux cannot reach for
/// another reason, such as its permissions, may still have a server.
fn nothing_to_list(error: &str) -> bool {
    let gone = ["No such file or directory", "Connection refused"];
    error.contains("no server running")
        || error.contains("no current target")
        || error.contains("error connecting to") && gone.iter().any(|cause| error.contains(cause))
}

fn parse_pane(line: &str) -> Option<Pane> {
    let mut fields = line.splitn(8, ' ');
    let mut next = || fields.next();
    let id = next()?.to_owned();
    let window = next()?.to_owned();
    let window_active = next()? == "1";
    let attached: u32 = next()?.parse().ok()?;
    let in_mode = next()? == "1";
    let pid = next()?.parse().ok()?;
    let instance = next()?.to_owned();
    let window_name = next()?.to_owned();
    Some(Pane {
        id,
        window,
        window_name,
        clients: if window_active { attached } else { 0 },
        in_mode,
        pid,
        instance,
    })
}

/// A listed session's ID and name.
fn parse_session(line: &str) -> Option<(&str, &str)> {
    line.split_once(' ')
}

fn parse_opened(output: &str) -> Option<Opened> {
    let mut fields = output.trim_end_matches('\n').splitn(3, ' ');
    Some(Opened {
        pane: fields.next()?.to_owned(),
        instance: fields.next()?.to_owned(),
        socket: fields.next()?.to_owned(),
    })
}

/// tmux panes carry no Shoal labels, so the daemon's records of the panes
/// Shoal created are the proof of ownership: discovery finds none, and a
/// close needs a recorded pane, which is Shoal's while it is open on the
/// server instance the record names.
pub(super) struct Cleanup;

impl super::Host for Cleanup {
    fn name(&self) -> super::HostName {
        super::HostName::Tmux
    }

    fn ensure_installed(&self) -> Result<()> {
        ensure!(
            Tool::Tmux.installed(),
            "tmux is not installed or not on PATH"
        );
        Ok(())
    }

    fn server(&self, data: &BTreeMap<String, String>) -> String {
        server(data)
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
            ensure!(place.owned, "Shoal did not create tmux pane {}", place.id());
            Tmux::recorded(&place.data)
                .close(place.id(), place.instance.as_deref())
                .await
        })
    }
}

#[cfg(test)]
mod tests;
