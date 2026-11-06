//! The terminal hosts Shoal knows, in the order they claim a terminal, and
//! how config sets each up: which one serves this terminal, which one is the
//! default, and where a new place goes beside the caller.
use std::io::IsTerminal;

use super::{
    herdr,
    host::{Attaches, Host, Places},
    rex, tmux, zellij, zmx,
};
use crate::{
    config::Effective,
    sessions::{HostName, Origin},
};

/// Hosts in the order they claim a terminal, which is [`HostName`]'s; a
/// terminal none of them serves goes to the default host.
fn registered() -> impl Iterator<Item = Registered> {
    HostName::ALL.iter().copied().filter_map(registration)
}

/// The host named `name` with the options its own config section gives;
/// none for the plain terminal.
fn registration(name: HostName) -> Option<Registered> {
    Some(match name {
        HostName::Herdr => Registered {
            host: &herdr::Herdr,
            options: |settings| herdr::options(&settings.terminal.herdr),
        },
        HostName::Rex => Registered {
            host: &rex::Rex,
            options: |settings| rex::options(&settings.terminal.rex),
        },
        HostName::Zmx => Registered {
            host: &zmx::Zmx,
            options: |_| Options::DEFAULT,
        },
        HostName::Tmux => Registered {
            host: &tmux::Tmux,
            options: |settings| {
                tmux::options(&settings.terminal.tmux, settings.terminal.default_host)
            },
        },
        HostName::Zellij => Registered {
            host: &zellij::Zellij,
            options: |settings| {
                zellij::options(&settings.terminal.zellij, settings.terminal.default_host)
            },
        },
        HostName::Terminal => return None,
    })
}

/// A host with the options `settings` give it.
struct Registered {
    host: &'static dyn Host,
    options: fn(&Effective) -> Options,
}

/// Every host, for what it does whatever config says.
pub(crate) fn hosts() -> impl Iterator<Item = &'static dyn Host> {
    registered().map(|registered| registered.host)
}

/// The hosts whose programs this process finds.
pub(crate) fn installed_hosts() -> Vec<HostName> {
    hosts()
        .filter(|host| host.ensure_installed().is_ok())
        .map(Host::name)
        .collect()
}

/// What config sets for a host, from the section of its own it may have.
pub(crate) struct Options {
    /// Whether the host serves the terminals it runs in.
    pub enabled: bool,
    /// Whether it opens places, such as tabs, beside the caller.
    pub opens_places: bool,
    /// Whether a place it opens from the caller's place splits that place
    /// instead.
    pub opens_splits: bool,
    /// The template for the names of the places it opens, in place of the
    /// default.
    pub name_template: Option<String>,
    /// The template for the name of the group it opens places in when it
    /// has none from the caller, for a host that keeps groups.
    pub group_template: Option<String>,
    /// Whether the places it opens for a workspace close once it is done,
    /// over the top-level `close_when_done`.
    pub close_when_done: Option<bool>,
}

impl Options {
    /// A host without a section: config only sets what all hosts share.
    const DEFAULT: Self = Self {
        enabled: true,
        opens_places: true,
        opens_splits: false,
        name_template: None,
        group_template: None,
        close_when_done: None,
    };
}

/// A host as `settings` set it up.
pub(crate) struct Configured {
    pub(crate) host: &'static dyn Host,
    pub(crate) options: Options,
    /// Whether the places it opens for a workspace close once it is done.
    pub(crate) closes_when_done: bool,
}

impl Configured {
    fn new(host: &'static dyn Host, options: Options, settings: &Effective) -> Self {
        let closes_when_done = options
            .close_when_done
            .unwrap_or(settings.terminal.close_when_done);
        Self {
            host,
            options,
            closes_when_done,
        }
    }

    /// Every host as `settings` set it up, in the order they claim a terminal.
    pub(crate) fn all(settings: &Effective) -> impl Iterator<Item = Self> + '_ {
        registered()
            .map(|registered| Self::new(registered.host, (registered.options)(settings), settings))
    }

    /// The host named `name`, or the plain terminal.
    pub(crate) fn named(settings: &Effective, name: HostName) -> Self {
        Self::all(settings)
            .find(|host| host.host.name() == name)
            .unwrap_or_else(|| Self::plain(settings))
    }

    /// The host `default_host` names for a terminal no host serves, unless
    /// config turns it off.
    pub(crate) fn default_host(settings: &Effective) -> Option<Self> {
        Self::all(settings)
            .find(|host| host.host.name() == settings.terminal.default_host && host.options.enabled)
    }

    /// The plain terminal, which every terminal falls back to.
    pub(crate) fn plain(settings: &Effective) -> Self {
        let options = Options {
            opens_places: false,
            ..Options::DEFAULT
        };
        Self::new(&Plain, options, settings)
    }

    /// Whether the host serves the terminal this process runs in.
    pub(crate) fn serves(&self) -> bool {
        self.options.enabled && self.host.serves()
    }

    /// The host as it serves a place of its own: one that opens no places
    /// beside its own keeps starts there in place.
    fn serving(mut self) -> Self {
        if self
            .host
            .places()
            .is_some_and(|places| !places.opens_beside_own())
        {
            self.options.opens_places = false;
        }
        self
    }

    /// Whether a queued start from `origin`, the place that queued it, joins
    /// its group here: a place of this host in a group, such as a Herdr tab
    /// in its workspace. A start queued from a place in no group, such as a
    /// zmx session, goes to the default host.
    pub(crate) fn takes_queued(&self, origin: &Origin) -> bool {
        self.options.enabled
            && self.places().is_some()
            && origin.host == self.host.name()
            && origin.group.is_some()
    }

    /// How the host opens places, where config lets it.
    pub(crate) fn places(&self) -> Option<&'static dyn Places> {
        self.options
            .opens_places
            .then(|| self.host.places())
            .flatten()
    }

    /// Where a start from this terminal goes in `origin`, the place it runs
    /// in or the group it joins: splitting that place where config asks for
    /// splits and there is one, otherwise in a place of its own.
    pub(crate) fn beside<'a>(&self, origin: Option<&'a Origin>) -> Beside<'a> {
        Beside {
            origin,
            split: origin
                .and_then(|origin| origin.place.as_deref())
                .filter(|_| self.options.opens_splits),
        }
    }
}

/// Where a new place goes: into `origin`'s group, splitting `split` when
/// set.
#[derive(Clone, Copy)]
pub(crate) struct Beside<'a> {
    pub(crate) origin: Option<&'a Origin>,
    pub(crate) split: Option<&'a str>,
}

impl<'a> Beside<'a> {
    /// A place of its own in `origin`'s group, as a start without the
    /// caller's place opens.
    pub(crate) fn opening(origin: Option<&'a Origin>) -> Self {
        Self {
            origin,
            split: None,
        }
    }
}

/// A terminal no host serves: everything runs in place.
struct Plain;

impl Host for Plain {
    fn name(&self) -> HostName {
        HostName::Terminal
    }

    fn serves(&self) -> bool {
        true
    }
}

/// The host serving this terminal under `settings`. A terminal no host
/// serves goes to the default host, and a process without a terminal, or
/// whose default host config turns off, runs in place.
pub(crate) fn current(settings: &Effective) -> Configured {
    match Configured::all(settings).find(Configured::serves) {
        Some(host) => host.serving(),
        None if on_terminal() => {
            Configured::default_host(settings).unwrap_or_else(|| Configured::plain(settings))
        }
        None => Configured::plain(settings),
    }
}

/// Whether an agent launched here runs in a session this terminal attaches
/// to, leaving the caller's shell where it was once the terminal detaches.
pub(crate) fn keeps_agents_in_sessions(settings: &Effective) -> bool {
    current(settings).host.attaches().is_some()
}

/// The program an agent launch here under `settings` needs besides the
/// agent's own: that of the host it runs in a session of.
pub(crate) fn launcher_program(settings: &Effective) -> Option<&'static str> {
    current(settings)
        .host
        .attaches()
        .map(Attaches::launcher_program)
}

/// Whether this process runs on a terminal.
fn on_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The variables naming places of hosts other than `host`, which a place
/// `host` opens must drop: Shoal there would take the caller's place for its
/// own, and record or close it. So must the marks of the space whose process
/// opens it, since what runs there marks its own space, and processes that
/// kept the opener's would count as the opener's, which its stop would end,
/// and its scope token, which a shell left in the place would keep.
pub(super) fn foreign_place_variables(host: HostName) -> Vec<&'static str> {
    use crate::env::{CHAT, EXECUTION_ID, RUN_ID, SCOPE_TOKEN, WORKSPACE_ID};
    let marks = [CHAT, EXECUTION_ID, WORKSPACE_ID, RUN_ID, SCOPE_TOKEN];
    hosts()
        .filter(|other| other.name() != host)
        .filter_map(Host::places)
        .flat_map(|other| other.place_variables().iter().copied())
        .chain(marks)
        .collect()
}

/// The host named `name`, where config enables it.
pub(crate) fn enabled_host(settings: &Effective, name: HostName) -> Option<&'static dyn Host> {
    Configured::all(settings)
        .find(|host| host.options.enabled && host.host.name() == name)
        .map(|host| host.host)
}

/// Whether the places `host` opens show a name.
pub(crate) fn names_places(host: &dyn Host) -> bool {
    host.places().is_some_and(Places::names_places)
}

/// Where `place`, a place its host opened for `name`, runs and how to reach
/// it, as a background start reports it; `None` for a host without places.
pub(crate) fn background_place(place: &Origin, name: &str) -> Option<String> {
    named(place.host)
        .places()
        .map(|places| places.background_place(name))
}

/// The host named `name`, or the plain terminal, which keeps places no
/// host does.
pub(crate) fn named(name: HostName) -> &'static dyn Host {
    hosts().find(|host| host.name() == name).unwrap_or(&Plain)
}

#[cfg(test)]
mod tests;
