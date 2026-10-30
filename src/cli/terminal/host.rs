//! What a terminal host does, and the work generic code hands it.
use std::{ffi::OsString, path::Path};

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

    /// How this host runs agents and chats in new sessions this terminal
    /// attaches to, unless it runs none so from here.
    fn attaches(&self) -> Option<&dyn Attaches> {
        None
    }

    /// The demos this host keeps, unless it keeps none.
    fn demos(&self) -> Option<&dyn Demos> {
        None
    }

    /// How this host types into the places it records, unless it types
    /// into none.
    fn types(&self) -> Option<&dyn Types> {
        None
    }
}

/// A host that opens places, such as tabs, windows or sessions, beside the
/// caller and runs Shoal work there. Generic code starts work through it only
/// where config lets the host open places; what a place this process runs in
/// tells, it reads whatever config says.
pub(in crate::cli) trait Places: Sync {
    /// Run `start` in a new place, returning the place: one of its own in
    /// the group of `start`'s origin, or splitting the place `start` names.
    /// A focused place becomes the host's current one; generic code shows
    /// it to a terminal the host does not serve.
    fn open<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<Origin>>;

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

    /// The variables that choose which server of this host, and which of its
    /// programs, a process reaches, which a start the daemon runs later for
    /// this process keeps.
    fn server_variables(&self) -> &'static [&'static str] {
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

    /// Bring `place`, one this host opened, called `name`, before the user
    /// of this terminal: where the terminal shows this host, by focusing it;
    /// otherwise by attaching the terminal to it until the user leaves it,
    /// or by showing it where the host shows its places.
    fn show<'a>(&'a self, place: &'a Origin, name: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Whether the places this host opens show a name.
    fn names_places(&self) -> bool {
        false
    }

    /// Show `label` as the name of `place`, one this host opened, where its
    /// places show names.
    fn rename_place<'a>(&'a self, _place: &'a str, _label: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Show `new` as the name of `place`, recorded where a chat runs, while
    /// the place still shows `old`, on the server the place records. A place
    /// that is gone or shows another name keeps it.
    fn relabel_place<'a>(
        &'a self,
        _place: &'a Origin,
        _old: &'a str,
        _new: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Whether a place this host opens for an addition keeps a shell that
    /// enters the workspace afterwards.
    fn places_keep_shells(&self) -> bool {
        true
    }

    /// Whether a start from a place of this host opens another place beside
    /// it; otherwise the start runs in place there.
    fn opens_beside_own(&self) -> bool {
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
/// finds where the daemon recorded them and shows as [`Places::show`] does.
pub(in crate::cli) trait Sessions: Sync {
    /// The session at `place`, where an agent or chat was recorded running,
    /// while it is open.
    fn session_at<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<Option<Session>>>;

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
}

/// A host that runs agents and chats launched here in new sessions of its
/// own this terminal attaches to, which their wrapper can type prompts into;
/// a focused addition stays here to start its agent so.
pub(in crate::cli) trait Attaches: Sessions {
    /// Run `start`, a tracked agent `agent` of `owner`, in a new session and
    /// attach this terminal to it, returning the session once the terminal
    /// left it.
    fn run_agent<'a>(
        &'a self,
        start: &'a Start<'a>,
        owner: Owner<'a>,
        agent: &'a str,
    ) -> BoxFuture<'a, Result<String>>;

    /// Run `start`, a chat, in a new session and attach this terminal to
    /// it, as [`Attaches::run_agent`] runs an agent.
    fn run_chat<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<String>>;

    /// The program an agent launch here needs besides the agent's own.
    fn launcher_program(&self) -> &'static str;
}

/// A host that types into the places it records, such as a demo's or the
/// place an agent runs in, for a process anywhere on its server.
pub(in crate::cli) trait Types: Sync {
    /// Type `keys` into `place` as they are, control characters included.
    fn send_keys<'a>(&'a self, place: &'a Origin, keys: &'a str) -> BoxFuture<'a, Result<()>>;
}

/// A host that keeps demos in places of its own, which the user opens as
/// [`Places::show`] shows a place and reads, and types into as [`Types`]
/// does.
pub(in crate::cli) trait Demos: Sync {
    /// Start `demo` in a new background place of this host, returning the
    /// place.
    fn start_demo<'a>(&'a self, demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Origin>>;

    /// What the demo at `place` shows, oldest line first.
    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>>;
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
