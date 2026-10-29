//! What a terminal host does, and the work generic code hands it.
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    path::Path,
};

use anyhow::Result;
use futures_util::future::BoxFuture;

use super::{Owner, OwnerKind, PlaceState, Session, Watched};
use crate::sessions::{HostName, Origin};

/// A terminal host: a multiplexer or terminal app that runs Shoal work beside
/// the caller. A capability a host lacks keeps that work in this process.
pub(in crate::cli) trait Host: Sync {
    /// The name records, sessions and messages use.
    fn name(&self) -> HostName;

    /// Whether this host serves the terminal this process runs in, where
    /// config enables it.
    fn serves(&self) -> bool;

    /// Whether a queued start from `origin`, the place that queued it, runs
    /// here, where config lets the host open places.
    fn takes_queued(&self, _origin: Option<&Origin>) -> bool {
        false
    }

    /// Refuse when the programs this host runs are missing.
    fn ensure_installed(&self) -> Result<()> {
        Ok(())
    }

    /// The group labelled `label` on the server of `near`, or this
    /// machine's, created at `cwd` when there is none; `None` for a host
    /// without groups.
    fn group<'a>(
        &'a self,
        _near: Option<&'a Origin>,
        _label: &'a str,
        _cwd: &'a Path,
    ) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async { Ok(None) })
    }

    /// Run `start`, a workspace addition, in a new place, returning whether
    /// it left this process. Generic code calls it only where config lets
    /// the host open places.
    fn start_add<'a>(&'a self, _start: &'a Start<'a>) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Whether this host runs Shoal for a workspace in the background, where
    /// config lets it open places.
    fn runs_in_background(&self) -> bool {
        false
    }

    /// Run `start` in a new background place.
    fn run_in_background<'a>(&'a self, _start: &'a Start<'a>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot run in the background", self.name()) })
    }

    /// Run `argv` with `env` in `place`, an earlier place of this host, when
    /// it is idle, for the state directory `state`. Returns whether it ran.
    fn run_in_place<'a>(
        &'a self,
        _state: &'a Path,
        _place: &'a Origin,
        _argv: &'a [OsString],
        _env: &'a [(&'a str, String)],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Run `start`, a tracked agent `agent` of `owner`, in a new session and
    /// attach this terminal to it, returning the session once the terminal
    /// left it. `None` means the agent runs in place.
    fn run_agent<'a>(
        &'a self,
        _start: &'a Start<'a>,
        _owner: Owner<'a>,
        _agent: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    /// Run `start`, a chat, beside this terminal, where config lets the host
    /// open places.
    fn start_chat<'a>(&'a self, _start: &'a Start<'a>) -> BoxFuture<'a, Result<ChatStart>> {
        Box::pin(async { Ok(ChatStart::Here) })
    }

    /// What several background starts run in.
    fn background_places(&self) -> &'static str;

    /// How a background start of `name` is reported: where it runs and how
    /// to reach it.
    fn background_note(&self, name: &str) -> String;

    // The capabilities below belong to the host this process runs in,
    // whichever host serves its terminal.

    /// The place this process runs in, with what a later start needs to
    /// open a place beside it.
    fn origin(&self) -> Option<Origin> {
        None
    }

    /// The variables that name this host's place for a process in it, which
    /// a place another host opens must not inherit.
    fn place_variables(&self) -> &'static [&'static str] {
        &[]
    }

    /// The place this process runs in, as [`Host::close_place`] names it.
    fn this_place(&self) -> Option<String> {
        None
    }

    /// Close `place`, one this host opened for a workspace, which the
    /// daemon keeps no record of.
    fn close_place<'a>(&'a self, _place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot close places", self.name()) })
    }

    /// Whether a place this host opened is Shoal's to close without the
    /// daemon's record of it. A host whose places carry no ownership of their
    /// own has only the record, and its places without one stay open.
    fn closes_unrecorded_places(&self) -> bool {
        true
    }

    /// What tells `place` apart from a later place under the same ID, for a
    /// host that reuses IDs.
    fn place_instance<'a>(&'a self, _place: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    /// Whether the watched `place` is still open, and its agent's turn as
    /// this host reads it.
    fn place_state<'a>(&'a self, _place: &'a Watched) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(async { anyhow::bail!("{} cannot read places", self.name()) })
    }

    /// Whether the places this host opens show a name.
    fn names_places(&self) -> bool {
        false
    }

    /// Show `label` as the name of `place`, one this host opened.
    fn rename_place<'a>(&'a self, _place: &'a str, _label: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Whether a place this host opens for an addition keeps a shell that
    /// enters the workspace afterwards.
    fn places_keep_shells(&self) -> bool {
        true
    }

    /// The session this process runs in, where a prompt can be typed.
    fn prompt_session(&self) -> Option<OsString> {
        None
    }

    /// Submit `text` as a prompt in `session`, from [`Host::prompt_session`].
    fn submit_prompt<'a>(
        &'a self,
        _session: &'a OsStr,
        _text: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot type prompts", self.name()) })
    }

    /// Label the session this process runs in, when it runs in one, so
    /// `shoal attach` finds `owner`'s agent there.
    fn label_current<'a>(
        &'a self,
        _state: &'a Path,
        _owner: Owner<'a>,
        _agent: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Whether agents launched here run in sessions their wrapper can type
    /// prompts into.
    fn keeps_agents_in_sessions(&self) -> bool {
        false
    }

    /// The program an agent launch here needs besides the agent's own.
    fn launcher_program(&self) -> Option<&'static str> {
        None
    }

    /// `owner`'s running agent sessions.
    fn sessions<'a>(&'a self, _owner: Owner<'a>) -> BoxFuture<'a, Result<Vec<Session>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// The IDs of the owners of `kind` with at least one agent session.
    fn session_owners(&self, _kind: OwnerKind) -> BoxFuture<'_, Result<HashSet<String>>> {
        Box::pin(async { Ok(HashSet::new()) })
    }

    /// The session at `place`, where an agent or chat was recorded running,
    /// while it is open.
    fn session_at<'a>(&'a self, _place: &'a Origin) -> BoxFuture<'a, Result<Option<Session>>> {
        Box::pin(async { Ok(None) })
    }

    /// Whether the places recorded for agents of this host are sessions
    /// [`Host::session_at`] finds.
    fn attaches_places(&self) -> bool {
        false
    }

    /// Attach this terminal to `session`, one of [`Host::sessions`] or
    /// [`Host::session_at`], until the user detaches or the session ends.
    fn attach<'a>(&'a self, _session: &'a Session) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} keeps no sessions", self.name()) })
    }

    /// Start `demo` in a new background place of this host. Returns the
    /// place, or `None` when this host keeps no demos.
    fn start_demo<'a>(&'a self, _demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async { Ok(None) })
    }

    /// Show the demo at `place` in this terminal until the user leaves it.
    fn open_demo<'a>(&'a self, _place: &'a Origin) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot show demos", self.name()) })
    }

    /// What the demo at `place` shows, oldest line first.
    fn demo_screen<'a>(&'a self, _place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { anyhow::bail!("{} cannot read a demo's screen", self.name()) })
    }

    /// Type `keys` into the demo at `place`.
    fn send_demo_keys<'a>(
        &'a self,
        _place: &'a Origin,
        _keys: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("{} cannot type into a demo", self.name()) })
    }
}

/// The demo worker's argv for the place it runs in.
pub(in crate::cli) type DemoWorker<'a> =
    dyn Fn(&Origin) -> Result<Vec<OsString>> + Send + Sync + 'a;

/// Work a host starts in a new place, which generic code describes in full,
/// so the host only carries it out.
pub(in crate::cli) struct Start<'a> {
    /// The daemon state directory that owns the place.
    pub state: &'a Path,
    /// The place the start comes from, whose group the new place joins.
    pub origin: Option<&'a Origin>,
    /// The directory the place opens at.
    pub cwd: &'a Path,
    /// What the place is called: a tab's label, a session's name.
    pub label: &'a str,
    /// Whether the place takes the caller's terminal.
    pub focus: bool,
    /// The command the place runs.
    pub argv: &'a [OsString],
    /// Variables the command needs from this process, since the place may
    /// start in an environment of its own.
    pub env: &'a [(&'a str, String)],
}

/// A demo a host starts in a new background place, as [`Start`] describes
/// other work; its command depends on the place's handle.
pub(in crate::cli) struct Demo<'a> {
    pub launch: &'a str,
    pub state: &'a Path,
    pub origin: Option<&'a Origin>,
    pub workspace: &'a str,
    pub cwd: &'a Path,
    pub label: &'a str,
    pub env: &'a [(&'a str, String)],
    pub worker: &'a DemoWorker<'a>,
}

/// Where a host started a chat beside this terminal.
pub(in crate::cli) enum ChatStart {
    /// Nowhere: it runs in place, in this process.
    Here,
    /// In a new tab.
    Tab,
    /// In this new session, which this terminal stayed attached to until the
    /// user detached or the chat ended.
    Session(String),
}
