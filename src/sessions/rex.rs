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

/// A window Shoal opened, with its only block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opened {
    pub window: String,
    pub block: String,
}

/// A shell window to open in a session.
pub(crate) struct Window<'a> {
    pub session: &'a str,
    pub label: &'a str,
    pub cwd: &'a Path,
    pub focus: bool,
    /// Typed into the shell once it starts.
    pub input: Option<&'a str>,
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
        let listed: Sessions = serde_json::from_slice(&self.run(&["ls", "--json"]).await?)
            .context("invalid Rex session list")?;
        if let Some(oldest) = oldest_labelled(&listed.sessions, label) {
            return Ok(oldest.to_owned());
        }
        let cwd = cwd
            .to_str()
            .context("the session's directory is not UTF-8")?;
        let created: Created = serde_json::from_slice(
            &self
                .run(&["session", "new", label, "-c", cwd, "--json"])
                .await?,
        )
        .context("invalid Rex session")?;
        Ok(created.session_id)
    }

    /// Open a shell window, returning it and its block.
    pub async fn open(&self, window: &Window<'_>) -> Result<Opened> {
        let mut options = json!({
            "cwd": window.cwd.to_str().context("the window's directory is not UTF-8")?,
        });
        if let Some(input) = window.input {
            options["initial_input"] = input.into();
        }
        let input = json!({
            "session_id": window.session,
            "focus": window.focus,
            "window_label": window.label,
            "layout": {"block": {"flavor": SHELL_FLAVOR, "options": options}},
        });
        let opened: NewWindow =
            serde_json::from_slice(&self.call("session.new_window", input).await?)
                .context("invalid Rex window")?;
        let block = opened
            .block_ids
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .context("Rex opened a window without a block")?;
        Ok(Opened {
            window: opened
                .window_id
                .context("Rex opened a window without an ID")?,
            block,
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
    /// running, and nothing in the foreground but that shell.
    pub async fn at_prompt(&self, block: &str) -> Result<bool> {
        let Some(output) = self
            .run_found(&["block", "call", "-b", block, "process"])
            .await?
        else {
            return Ok(false);
        };
        let processes: Processes =
            serde_json::from_slice(&output).context("invalid Rex process list")?;
        Ok(processes.shell_in_foreground())
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
        let clients: Clients =
            serde_json::from_slice(&self.run(&["client", "ls", "--json"]).await?)
                .context("invalid Rex client list")?;
        let Some(app) = clients.mac_app() else {
            return Ok(false);
        };
        let mut select = vec![
            "-C".to_owned(),
            app.to_owned(),
            "do".to_owned(),
            "session.select".to_owned(),
            format!("session_id={}", block.session),
        ];
        if let Some(window) = &block.window {
            select.push(format!("window_id={window}"));
        }
        let select: Vec<_> = select.iter().map(String::as_str).collect();
        self.run(&select).await?;
        Ok(true)
    }

    /// Name `block`'s window `label`.
    pub async fn rename_window(&self, block: &Block, label: &str) -> Result<()> {
        let window = block.window.as_deref().context("the block has no window")?;
        let input = json!({"session_id": block.session, "window_id": window, "label": label});
        self.call("session.set_window_label", input).await?;
        Ok(())
    }

    /// Close `block` and nothing else, leaving its window to the user's other
    /// blocks; a window it leaves empty closes too. A block already gone is
    /// closed.
    pub async fn close(&self, id: &str) -> Result<()> {
        let Some(block) = self.block(id).await? else {
            return Ok(());
        };
        self.run_found(&["block", "close", id]).await?;
        if let Some(window) = &block.window
            && self.window_is_empty(&block.session, window).await?
        {
            self.run_found(&["window", "close", "-s", &block.session, window])
                .await?;
        }
        ensure!(
            self.block(id).await?.is_none(),
            "Rex block {id} is still open after closing it"
        );
        Ok(())
    }

    /// Whether `window` is still open without any block.
    async fn window_is_empty(&self, session: &str, window: &str) -> Result<bool> {
        let Some(windows) = self
            .run_found(&["window", "ls", "-s", session, "--json"])
            .await?
        else {
            return Ok(false);
        };
        let windows: Windows =
            serde_json::from_slice(&windows).context("invalid Rex window list")?;
        if !windows.windows.iter().any(|open| open.window_id == window) {
            return Ok(false);
        }
        let Some(blocks) = self
            .run_found(&["block", "ls", "-s", session, "--json"])
            .await?
        else {
            return Ok(false);
        };
        let blocks: Blocks = serde_json::from_slice(&blocks).context("invalid Rex block list")?;
        Ok(!blocks
            .blocks
            .iter()
            .any(|block| block.window_id.as_deref() == Some(window)))
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
struct NewWindow {
    window_id: Option<String>,
    block_ids: Option<Vec<Option<String>>>,
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
}

#[derive(Deserialize)]
struct Blocks {
    blocks: Vec<BlockInfo>,
}

#[derive(Deserialize)]
struct BlockInfo {
    window_id: Option<String>,
}

/// A terminal's processes: the program Rex started, what runs in the
/// foreground, and that process's ancestors up to the program.
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
}

impl Processes {
    /// The shell is in the foreground when nothing but the program Rex
    /// started, such as `login`, stands between it and the terminal.
    fn shell_in_foreground(&self) -> bool {
        let (Some(child), Some(_)) = (&self.child, &self.foreground) else {
            return false;
        };
        self.last_exit.is_none()
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
}

#[cfg(test)]
mod tests;
