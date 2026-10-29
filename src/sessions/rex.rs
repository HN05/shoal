//! The Rex CLI against one Rex server, for the terminal host and the
//! daemon's cleanup alike. Rex never reuses IDs, so session, window and
//! block IDs identify places; labels only find a session by name.
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use crate::subprocess::Run;

/// The oldest API revision with the methods Shoal calls.
const REVISION: u64 = 7;
/// `rex`'s exit status when the session, window or block it names is gone.
const NOT_FOUND: i32 = 3;
/// What a Rex origin's data records: the server's address, when not the
/// local default, and the directory of the `rex` that reaches it.
const SERVER: &str = "server";
const BIN_DIR: &str = "bin_dir";
/// Where the Mac app installs `rex`, below the home directory.
const APP_BIN_DIR: &str = "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS";
const SHELL_FLAVOR: &str = "com.superlogical.terminal.shell";

/// The address of the server `data` records, empty for the local default;
/// which `rex` reaches it does not tell servers apart.
pub(crate) fn server(data: &BTreeMap<String, String>) -> String {
    data.get(SERVER).cloned().unwrap_or_default()
}

/// A Rex server and the `rex` that reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rex {
    program: PathBuf,
    server: Option<String>,
}

/// A block and where it sits now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Block {
    pub session: String,
    pub window: Option<String>,
}

/// A block Shoal opened, with the window it opened for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opened {
    pub window: Option<String>,
    pub block: String,
}

/// A shell block to open in a session.
pub(crate) struct Shell<'a> {
    pub session: &'a str,
    pub label: &'a str,
    pub cwd: &'a Path,
    pub focus: bool,
    /// Typed into the shell once it starts.
    pub input: Option<&'a str>,
    /// The block to split for it; `None` opens a window of its own.
    pub beside: Option<&'a str>,
}

impl Shell<'_> {
    /// The layout node that starts the shell.
    fn layout(&self) -> Result<serde_json::Value> {
        let mut options = json!({
            "cwd": self.cwd.to_str().context("the shell's directory is not UTF-8")?,
        });
        if let Some(input) = self.input {
            options["initial_input"] = input.into();
        }
        Ok(json!({"block": {"flavor": SHELL_FLAVOR, "label": self.label, "options": options}}))
    }
}

impl Rex {
    /// The server and `rex` that origin `data` records.
    pub fn recorded(data: &BTreeMap<String, String>) -> Result<Self> {
        Ok(Self {
            program: program(data.get(BIN_DIR).map(PathBuf::from))?,
            server: data.get(SERVER).cloned(),
        })
    }

    /// The server this process reaches: the one Rex named in its
    /// environment, otherwise the local default.
    pub fn here() -> Result<Self> {
        Ok(Self {
            program: program(var("REX_BIN_DIR").map(PathBuf::from))?,
            server: var("REX_SERVER"),
        })
    }

    /// What an origin records to reach this server again.
    pub fn data(&self) -> BTreeMap<String, String> {
        let mut data = BTreeMap::new();
        if let Some(directory) = self.program.parent().and_then(Path::to_str) {
            data.insert(BIN_DIR.to_owned(), directory.to_owned());
        }
        if let Some(server) = &self.server {
            data.insert(SERVER.to_owned(), server.clone());
        }
        data
    }

    /// A `rex` command for this server, which never starts one.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .arg("--autostart=false")
            .stdin(std::process::Stdio::null());
        match &self.server {
            Some(server) => command.arg("--server").arg(server),
            // A REX_SERVER this process inherited names another server.
            None => command.env_remove("REX_SERVER"),
        };
        command
    }

    async fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut command = self.command();
        command.args(args);
        Ok(Run::new(command).checked().await?.stdout)
    }

    /// Like [`Self::run`], `None` when what `args` name is gone.
    async fn run_found(&self, args: &[&str]) -> Result<Option<Vec<u8>>> {
        let mut command = self.command();
        command.args(args);
        let output = Run::new(command).capture().await?;
        match output.status.code() {
            Some(0) => Ok(Some(output.stdout)),
            Some(NOT_FOUND) => Ok(None),
            _ => bail!(
                "rex {} failed: {}",
                args.join(" "),
                crate::subprocess::diagnostic(&output.stderr)
            ),
        }
    }

    async fn call(&self, method: &str, input: serde_json::Value) -> Result<Vec<u8>> {
        self.run(&["api", "call", method, &input.to_string()]).await
    }

    /// Refuse a `rex` older than the API revision Shoal needs.
    pub async fn ensure_supported(&self) -> Result<()> {
        let version: Version = serde_json::from_slice(&self.run(&["version", "--json"]).await?)
            .context("invalid rex version")?;
        ensure!(
            version.api_revision >= REVISION,
            "Rex {} has API revision {}; Shoal needs revision {REVISION} or later, so update Rex",
            version.rex_version,
            version.api_revision
        );
        Ok(())
    }

    /// The oldest session labelled `label`, created at `cwd` when there is
    /// none.
    pub async fn session(&self, label: &str, cwd: &Path) -> Result<String> {
        match oldest_labelled(&self.sessions().await?, label) {
            Some(oldest) => Ok(oldest.to_owned()),
            None => self.create_session(label, cwd).await,
        }
    }

    /// Every session on the server.
    async fn sessions(&self) -> Result<Vec<Session>> {
        let listed: Sessions = serde_json::from_slice(&self.run(&["ls", "--json"]).await?)
            .context("invalid Rex session list")?;
        Ok(listed.sessions)
    }

    /// A new session labelled `label` at `cwd`.
    async fn create_session(&self, label: &str, cwd: &Path) -> Result<String> {
        let cwd = cwd
            .to_str()
            .context("the session's directory is not UTF-8")?;
        let output = self
            .run(&["session", "new", label, "-c", cwd, "--json"])
            .await?;
        let created: Created = serde_json::from_slice(&output).context("invalid Rex session")?;
        Ok(created.session_id)
    }

    /// Open `shell` in a window of its own, or split beside the block it
    /// names.
    pub async fn open(&self, shell: &Shell<'_>) -> Result<Opened> {
        match shell.beside {
            Some(anchor) => self.split(shell, anchor).await,
            None => self.open_window(shell).await,
        }
    }

    /// Open `shell` in a new window labelled as the shell is.
    async fn open_window(&self, shell: &Shell<'_>) -> Result<Opened> {
        let input = json!({
            "session_id": shell.session,
            "focus": shell.focus,
            "window_label": shell.label,
            "layout": shell.layout()?,
        });
        let opened: NewBlocks =
            serde_json::from_slice(&self.call("session.new_window", input).await?)
                .context("invalid Rex window")?;
        let window = opened
            .window_id
            .clone()
            .context("Rex opened a window without an ID")?;
        Ok(Opened {
            window: Some(window),
            block: opened.block()?,
        })
    }

    /// Open `shell` to the right of `anchor`, splitting the anchor's space.
    async fn split(&self, shell: &Shell<'_>, anchor: &str) -> Result<Opened> {
        let input = json!({
            "session_id": shell.session,
            "anchor_id": anchor,
            "direction": "horizontal",
            "side": "after",
            "focus": shell.focus,
            "layout": shell.layout()?,
        });
        let opened: NewBlocks =
            serde_json::from_slice(&self.call("session.new_split", input).await?)
                .context("invalid Rex split")?;
        Ok(Opened {
            window: None,
            block: opened.block()?,
        })
    }

    /// Where `block` sits now, or `None` once it is gone.
    pub async fn block(&self, block: &str) -> Result<Option<Block>> {
        let Some(output) = self
            .run_found(&["block", "inspect", block, "--json"])
            .await?
        else {
            return Ok(None);
        };
        let inspected: Inspected = serde_json::from_slice(&output).context("invalid Rex block")?;
        Ok(Some(Block {
            session: inspected.session_id,
            window: inspected.block.window_id,
        }))
    }

    /// Whether `block`'s shell is back at its prompt: open, its program
    /// running, and nothing in the foreground but that shell. A block that is
    /// gone or unreadable, as on a server that cannot be reached, is not.
    pub async fn at_prompt(&self, block: &str) -> bool {
        let Ok(Some(output)) = self
            .run_found(&["block", "call", "-b", block, "process"])
            .await
        else {
            return false;
        };
        serde_json::from_slice::<Processes>(&output)
            .is_ok_and(|processes| processes.shell_in_foreground())
    }

    /// Type `text` into `block` exactly as given.
    pub async fn send(&self, block: &str, text: &str) -> Result<()> {
        self.run(&["send", "-b", block, text]).await?;
        Ok(())
    }

    /// `block`'s screen and scrollback as text.
    pub async fn capture(&self, block: &str) -> Result<String> {
        let output = self.run(&["capture", "-b", block, "--trim"]).await?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    /// Make `block`'s window the active one of `session`, focused there.
    pub async fn focus(&self, session: &str, block: &str) -> Result<()> {
        self.run(&["focus", "-s", session, block]).await?;
        Ok(())
    }

    /// Show `block`'s window in the connected Mac app, returning whether one
    /// was connected.
    pub async fn show_in_app(&self, block: &Block) -> Result<bool> {
        let clients = self.clients().await?;
        let Some(app) = clients.mac_app() else {
            return Ok(false);
        };
        self.select(app, block).await?;
        Ok(true)
    }

    /// How many connected apps show `session`, such as a Mac or iOS app
    /// with it open.
    pub async fn showing(&self, session: &str) -> Result<u32> {
        Ok(self.clients().await?.showing(session))
    }

    /// The clients connected to the server.
    async fn clients(&self) -> Result<Clients> {
        let output = self.run(&["client", "ls", "--json"]).await?;
        serde_json::from_slice(&output).context("invalid Rex client list")
    }

    /// Have the client `app` show `block`'s session and window.
    async fn select(&self, app: &str, block: &Block) -> Result<()> {
        let session = format!("session_id={}", block.session);
        let window = block
            .window
            .as_ref()
            .map(|window| format!("window_id={window}"));
        let mut select = vec!["-C", app, "do", "session.select", &session];
        select.extend(window.as_deref());
        self.run(&select).await?;
        Ok(())
    }

    /// What names `id`, a block that sits at `block`: its window's label
    /// while it is alone there, otherwise its own label.
    pub async fn name(&self, id: &str, block: &Block) -> Result<Option<String>> {
        Ok(self
            .window_of(block)
            .await?
            .and_then(|window| window.name_of(id)))
    }

    /// Name `id`, a block that sits at `block`, `label`, and its window too
    /// while the block is alone there, as [`Self::name`] reads it.
    pub async fn rename(&self, id: &str, block: &Block, label: &str) -> Result<()> {
        self.set_block_label(&block.session, id, label).await?;
        let Some(window) = self.window_of(block).await? else {
            return Ok(());
        };
        if window.holds_only(id) {
            self.set_window_label(&block.session, &window.window_id, label)
                .await?;
        }
        Ok(())
    }

    /// Label block `id` of `session` `label`.
    async fn set_block_label(&self, session: &str, id: &str, label: &str) -> Result<()> {
        let input = json!({"session_id": session, "block_id": id, "label": label});
        self.call("session.set_block_label", input).await?;
        Ok(())
    }

    /// Label `window` of `session` `label`.
    async fn set_window_label(&self, session: &str, window: &str, label: &str) -> Result<()> {
        let input = json!({"session_id": session, "window_id": window, "label": label});
        self.call("session.set_window_label", input).await?;
        Ok(())
    }

    /// The window `block` sits in, while it is open.
    async fn window_of(&self, block: &Block) -> Result<Option<WindowInfo>> {
        let Some(window) = &block.window else {
            return Ok(None);
        };
        let Some(windows) = self
            .run_found(&["window", "ls", "-s", &block.session, "--json"])
            .await?
        else {
            return Ok(None);
        };
        let windows: Windows =
            serde_json::from_slice(&windows).context("invalid Rex window list")?;
        Ok(windows
            .windows
            .into_iter()
            .find(|open| open.window_id == *window))
    }

    /// Close `block` and nothing else; Rex removes a window once its last
    /// block closes, so the user's other blocks in it stay. A block already
    /// gone is closed.
    pub async fn close(&self, id: &str) -> Result<()> {
        if self.block(id).await?.is_none() {
            return Ok(());
        }
        self.run_found(&["block", "close", id]).await?;
        ensure!(
            self.block(id).await?.is_none(),
            "Rex block {id} is still open after closing it"
        );
        Ok(())
    }
}

/// Rex blocks carry no ownership marks, so the daemon's records of the blocks
/// Shoal created are the only proof of ownership: discovery finds none, and
/// a close needs a recorded block. Rex never reuses IDs, so a recorded block
/// that still exists is the one Shoal created.
pub(super) struct Cleanup;

impl super::Host for Cleanup {
    fn name(&self) -> super::HostName {
        super::HostName::Rex
    }

    fn sessions<'a>(
        &'a self,
        _state: &'a Path,
    ) -> futures_util::future::BoxFuture<'a, Result<Vec<super::Session>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn close<'a>(
        &'a self,
        _state: &'a Path,
        place: &'a super::Origin,
        _owner: &'a super::Owner,
    ) -> futures_util::future::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            anyhow::ensure!(place.owned, "Shoal did not create Rex block {}", place.id());
            Rex::recorded(&place.data)?.close(place.id()).await
        })
    }
}

/// The value of variable `name`, when set and not empty.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// `rex` in Rex's bin directory when known, otherwise on PATH, otherwise
/// where the Mac app installs it.
fn program(bin_dir: Option<PathBuf>) -> Result<PathBuf> {
    let in_bin_dir = bin_dir.map(|directory| directory.join("rex"));
    let on_path = || {
        crate::fsutil::find_executable(
            OsStr::new("rex"),
            &std::env::var_os("PATH").unwrap_or_default(),
        )
    };
    let in_app = || {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(APP_BIN_DIR).join("rex"))
            .filter(|path| path.is_file())
    };
    in_bin_dir
        .filter(|path| path.is_file())
        .or_else(on_path)
        .or_else(in_app)
        .context("Rex is not installed: `rex` is not on PATH or in the Rex app")
}

/// The oldest of `sessions` labelled `label`; Rex's IDs grow with time.
fn oldest_labelled<'a>(sessions: &'a [Session], label: &str) -> Option<&'a str> {
    sessions
        .iter()
        .filter(|session| session.label.as_deref() == Some(label))
        .map(|session| session.session_id.as_str())
        .min()
}

#[derive(Deserialize)]
struct Version {
    rex_version: String,
    api_revision: u64,
}

#[derive(Deserialize)]
struct Sessions {
    sessions: Vec<Session>,
}

#[derive(Deserialize)]
struct Session {
    session_id: String,
    label: Option<String>,
}

#[derive(Deserialize)]
struct Created {
    session_id: String,
}

#[derive(Deserialize)]
struct NewBlocks {
    /// Set when Rex opened a window for the blocks.
    #[serde(default)]
    window_id: Option<String>,
    block_ids: Option<Vec<Option<String>>>,
}

impl NewBlocks {
    /// The only new block, a shell.
    fn block(self) -> Result<String> {
        self.block_ids
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .context("Rex opened no block")
    }
}

#[derive(Deserialize)]
struct Inspected {
    session_id: String,
    block: BlockInfo,
}

#[derive(Deserialize)]
struct Windows {
    windows: Vec<WindowInfo>,
}

#[derive(Deserialize)]
struct WindowInfo {
    window_id: String,
    label: Option<String>,
    #[serde(default)]
    layers: Vec<Layer>,
}

#[derive(Deserialize)]
struct Layer {
    #[serde(default)]
    blocks: Vec<LabelledBlock>,
}

#[derive(Deserialize)]
struct LabelledBlock {
    block_id: String,
    label: Option<String>,
}

impl WindowInfo {
    /// Every block in the window, whatever layer holds it.
    fn blocks(&self) -> impl Iterator<Item = &LabelledBlock> {
        self.layers.iter().flat_map(|layer| &layer.blocks)
    }

    /// Whether block `id` is the window's only block.
    fn holds_only(&self, id: &str) -> bool {
        let mut blocks = self.blocks();
        blocks.next().is_some_and(|block| block.block_id == id) && blocks.next().is_none()
    }

    /// What names block `id` here: the window's label while the block is
    /// alone in it, otherwise the block's own label.
    fn name_of(&self, id: &str) -> Option<String> {
        if self.holds_only(id) {
            return self.label.clone();
        }
        self.blocks()
            .find(|block| block.block_id == id)
            .and_then(|block| block.label.clone())
    }
}

#[derive(Deserialize)]
struct BlockInfo {
    window_id: Option<String>,
}

/// A terminal's processes: the program Rex started, what runs in the
/// foreground, and that process's ancestors up to the program. The shell is
/// at its prompt when it is the foreground process itself.
#[derive(Deserialize)]
struct Processes {
    last_exit: Option<serde_json::Value>,
    child: Option<Process>,
    foreground: Option<Process>,
    #[serde(default)]
    ancestors: Vec<Process>,
}

#[derive(Deserialize)]
struct Process {
    pid: i64,
    #[serde(default)]
    name: String,
}

/// Programs that are shells, by name; a shell that ran `exec` keeps its
/// process but not its name.
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh",
];

impl Processes {
    /// The shell is in the foreground when nothing but the program Rex
    /// started, such as `login`, stands between it and the terminal.
    fn shell_in_foreground(&self) -> bool {
        let (Some(child), Some(foreground)) = (&self.child, &self.foreground) else {
            return false;
        };
        self.last_exit.is_none()
            && SHELLS.contains(&foreground.name.trim_start_matches('-'))
            && self
                .ancestors
                .iter()
                .all(|ancestor| ancestor.pid == child.pid)
    }
}

#[derive(Deserialize)]
struct Clients {
    clients: Vec<Client>,
}

#[derive(Deserialize)]
struct Client {
    client_id: String,
    info: Option<ClientInfo>,
    connection_state: Option<String>,
    #[serde(default)]
    session_ids: Vec<String>,
}

#[derive(Deserialize)]
struct ClientInfo {
    kind: String,
    platform: Option<String>,
}

impl Clients {
    /// The connected Mac app, which shows what Shoal selects for the user.
    fn mac_app(&self) -> Option<&str> {
        self.clients
            .iter()
            .find(|client| {
                client.connection_state.as_deref() == Some("connected")
                    && client.info.as_ref().is_some_and(|info| {
                        info.kind == "app" && info.platform.as_deref() == Some("macos")
                    })
            })
            .map(|client| client.client_id.as_str())
    }

    /// How many connected apps show `session`.
    fn showing(&self, session: &str) -> u32 {
        let count = self
            .clients
            .iter()
            .filter(|client| {
                client.connection_state.as_deref() == Some("connected")
                    && client.info.as_ref().is_some_and(|info| info.kind == "app")
                    && client.session_ids.iter().any(|id| id == session)
            })
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests;
