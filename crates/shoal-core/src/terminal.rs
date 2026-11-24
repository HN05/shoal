//! The terminal hosts Shoal runs work in: the [`Host`] trait, the
//! capability traits in [`host`] a host may support, the registry that
//! chooses one under config, and the hosts themselves. Generic code never
//! names a host; adding one means naming it in
//! [`HostName`](crate::sessions::HostName), implementing [`Host`] and the
//! capability traits it supports, and registering it where the compiler then
//! asks; a `default_host` value of its name makes it the host for terminals
//! no host serves.
mod environment;
pub mod groups;
mod herdr;
mod here;
mod host;
pub mod hosts;
mod labelled;
mod naming;
mod opening;
mod placement;
mod prompts;
pub mod queued;
mod recorded;
mod rex;
mod sessions;
mod tmux;
mod typed;
mod zellij;
mod zmx;

pub use environment::{detached_start_environment, start_environment};
pub use here::{current_origin, current_place, origin_here, with_instance};
#[cfg(any(test, feature = "test-support"))]
pub use host::Closes;
pub use host::{
    Attaches, Demo, DemoWorker, Demos, Elsewhere, Host, NOT_INHERITED_BY_DEMOS, Nested, Nests,
    PlaceState, Places, Sessions, Start, Watched, Watches,
};
pub(crate) use hosts::launcher_program;
pub use hosts::{
    Beside, Configured, Options, background_place, current, enabled_host, hosts, installed_hosts,
    keeps_agents_in_sessions, named, names_places,
};
pub use labelled::labelled_group;
pub use naming::PlaceName;
pub use opening::{
    chat_place_env, open_in_background, place_env, rerun_in_host, shoal_command, show_opened,
    show_place_elsewhere,
};
pub use placement::Placement;
pub use prompts::PromptSession;
pub(crate) use prompts::agents_can_be_prompted;
pub use prompts::{WAKE_PROMPT, send_keys, submit_prompt, submitted_by_shoal};
pub use recorded::{
    KnownSessions, Recorded, attach, known_sessions, left_open, open_session_owners, report_detach,
    session_owners, sessions, show_elsewhere, shown_elsewhere, shown_here,
};
pub(crate) use sessions::OwnerKind;
pub use sessions::{Owner, Session, chat_session_name};
pub use typed::run as run_launch;
