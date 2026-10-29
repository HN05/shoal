//! What a terminal host does, and the work generic code hands it.
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use anyhow::Result;
use futures_util::future::BoxFuture;

use super::{Owner, PlaceState, Session, Watched};
use crate::sessions::{HostName, Origin};

/// A terminal host: a multiplexer or terminal app that runs Shoal work beside
/// the caller. Each capability it has is a trait of its own, which it hands
/// out; generic code keeps work a host lacks the capability for in this
/// process, or does it its own way.
pub(in crate::cli) trait Host: Sync {
    /// The name records, sessions and messages use.
    fn name(&self) -> HostName;

    /// Whether this host serves the terminal this process runs in, where
    /// config enables it.
    fn serves(&self) -> bool;

    /// Refuse when the programs this host runs are missing.
    fn ensure_installed(&self) -> Result<()> {
        Ok(())
    }

    /// How this host opens places beside the caller and runs work there,
    /// unless it opens none.
    fn places(&self) -> Option<&dyn Places> {
        None
    }

    /// How this host reads the places it opened, so a watcher closes them
    /// with their workspace, unless it reads none.
    fn watches(&self) -> Option<&dyn Watches> {
        None
    }

    /// How this host closes the places it opened without the daemon's
    /// record of them, unless only that record closes them.
    fn closes(&self) -> Option<&dyn Closes> {
        None
    }

    /// The agent sessions this host keeps, unless it keeps none.
    fn sessions(&self) -> Option<&dyn Sessions> {
        None
    }

    /// How this host types prompts, unless it types none.
    fn prompts(&self) -> Option<&dyn Prompts> {
        None
    }

    /// The demos this host keeps, unless it keeps none.
    fn demos(&self) -> Option<&dyn Demos> {
        None
    }
}

/// A host that opens places, such as tabs, windows or sessions, beside the
/// caller and runs Shoal work there. Generic code starts work through it only
/// where config lets the host open places; what a place this process runs in
/// tells, it reads whatever config says.
pub(in crate::cli) trait Places: Sync {
    /// Whether a queued start from `origin`, the place that queued it, runs
    /// here.
    fn takes_queued(&self, origin: Option<&Origin>) -> bool;

    /// Run `start`, a workspace addition, in a new place, returning whether
    /// it left this process.
    fn start_add<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<bool>>;

    /// Run `start`, Shoal for a workspace, in a new background place.
    fn run_in_background<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<()>>;

    /// Run `start`, a chat, beside this terminal.
    fn start_chat<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<ChatStart>>;

    /// What several background starts run in.
    fn background_places(&self) -> &'static str;

    /// How a background start of `name` is reported: where it runs and how
    /// to reach it.
    fn background_note(&self, name: &str) -> String;

    /// The place this process runs in, under the one ID [`Watches`] and
    /// [`Closes`] name it by, with what a later start needs to open a place
    /// beside it.
    fn origin(&self) -> Option<Origin> {
        None
    }

    /// The variables that name this host's place for a process in it, which
    /// a place another host opens must not inherit.
    fn place_variables(&self) -> &'static [&'static str] {
        &[]
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

    /// Run `argv` with `env` in `place`, an earlier place of this host, when
    /// it is idle, for the state directory `state`. Returns whether it ran;
    /// a host whose places end with their work never does.
    fn run_in_place<'a>(
        &'a self,
        _state: &'a Path,
        _place: &'a Origin,
        _argv: &'a [OsString],
        _env: &'a [(&'a str, String)],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Whether the places this host opens show a name.
    fn names_places(&self) -> bool {
        false
    }

    /// Show `label` as the name of `place`, one this host opened, where its
    /// places show names.
    fn rename_place<'a>(&'a self, _place: &'a str, _label: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Whether a place this host opens for an addition keeps a shell that
    /// enters the workspace afterwards.
    fn places_keep_shells(&self) -> bool {
        true
    }
}

/// A host that reads the places it opened for workspaces, so a watcher
/// closes each once its workspace is done.
pub(in crate::cli) trait Watches: Sync {
    /// Whether the watched `place` is still open, and its agent's turn as
    /// this host reads it.
    fn place_state<'a>(&'a self, place: &'a Watched) -> BoxFuture<'a, Result<PlaceState>>;

    /// What tells `place` apart from a later place under the same ID, for a
    /// host that reuses IDs.
    fn place_instance<'a>(&'a self, _place: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async { Ok(None) })
    }

    /// Whether closing `place`, one this host's watcher closes, ends `here`,
    /// the place this process runs in.
    fn closing_ends<'a>(&'a self, place: &'a str, here: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { place == here })
    }
}

/// A host whose places carry their own ownership, so Shoal closes one the
/// daemon keeps no record of or cannot close. A host without it has only the
/// record, and its places without one stay open.
pub(in crate::cli) trait Closes: Sync {
    /// Close `place`, one this host opened for a workspace, where the daemon
    /// leaves that to the host.
    fn close_place<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<()>>;
}

/// A host that keeps agents in sessions of its own, which `shoal attach`
/// finds where the daemon recorded them and attaches this terminal to.
pub(in crate::cli) trait Sessions: Sync {
    /// Run `start`, a tracked agent `agent` of `owner`, in a new session and
    /// attach this terminal to it, returning the session once the terminal
    /// left it. `None` means the agent runs in place.
    fn run_agent<'a>(
        &'a self,
        start: &'a Start<'a>,
        owner: Owner<'a>,
        agent: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>>>;

    /// Attach this terminal to `session`, one [`Sessions::session_at`]
    /// found, until the user detaches or the session ends.
    fn attach<'a>(&'a self, session: &'a Session) -> BoxFuture<'a, Result<()>>;

    /// Whether `session`, which this host attached this terminal to, still
    /// runs after the terminal left it. A host that shows its sessions
    /// elsewhere never attaches the terminal.
    fn left_running<'a>(&'a self, _session: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    /// Mark the session this process runs in, when it runs in one, as
    /// `owner`'s, so cleanup closes it with `owner` rather than with the
    /// owner it was created for.
    fn label_current<'a>(
        &'a self,
        _state: &'a Path,
        _owner: Owner<'a>,
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

    /// The session at `place`, where an agent or chat was recorded running,
    /// while it is open.
    fn session_at<'a>(&'a self, _place: &'a Origin) -> BoxFuture<'a, Result<Option<Session>>> {
        Box::pin(async { Ok(None) })
    }

    /// The session open at each of `places`, in order, for a host that reads
    /// them all at once more cheaply than one at a time.
    fn sessions_at<'a>(
        &'a self,
        places: &'a [&'a Origin],
    ) -> BoxFuture<'a, Result<Vec<Option<Session>>>> {
        Box::pin(async move {
            let mut found = Vec::with_capacity(places.len());
            for place in places {
                found.push(self.session_at(place).await?);
            }
            Ok(found)
        })
    }

    /// Whether the places recorded for agents of this host are sessions
    /// [`Sessions::session_at`] finds.
    fn attaches_places(&self) -> bool {
        false
    }
}

/// A host that types prompts into the session an agent runs in, for the
/// agent's wrapper.
pub(in crate::cli) trait Prompts: Sync {
    /// The session this process runs in, where a prompt can be typed.
    fn prompt_session(&self) -> Option<OsString>;

    /// Submit `text` as a prompt in `session`, from
    /// [`Prompts::prompt_session`].
    fn submit_prompt<'a>(&'a self, session: &'a OsStr, text: &'a str) -> BoxFuture<'a, Result<()>>;
}

/// A host that keeps demos in places of its own, which the user opens, reads
/// and types into.
pub(in crate::cli) trait Demos: Sync {
    /// Start `demo` in a new background place of this host, returning the
    /// place.
    fn start_demo<'a>(&'a self, demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Origin>>;

    /// Show the demo at `place` in this terminal until the user leaves it.
    fn open_demo<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<()>>;

    /// What the demo at `place` shows, oldest line first.
    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>>;

    /// Type `keys` into the demo at `place`.
    fn send_demo_keys<'a>(&'a self, place: &'a Origin, keys: &'a str) -> BoxFuture<'a, Result<()>>;
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
    /// The caller's place, which the new place splits; `None` opens a place
    /// of its own.
    pub split: Option<&'a str>,
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
    /// In a new tab.
    Tab,
    /// In this new session, which this terminal stayed attached to until the
    /// user detached or the chat ended.
    Session(String),
}
