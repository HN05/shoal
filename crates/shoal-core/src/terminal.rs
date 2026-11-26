//! The terminal hosts Shoal runs work in, and the registry that chooses one
//! under config. Generic code never names a host. See
//! `docs/design/terminals.md`.
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
pub(crate) use host::Sessions;
pub use host::{
    Attaches, Demo, DemoWorker, Demos, Elsewhere, Host, Nested, PlaceState, Places, Start, Watched,
    Watches,
};
pub use hosts::{
    Beside, Configured, Options, background_place, current, hosts, installed_hosts,
    keeps_agents_in_sessions, named, names_places,
};
pub(crate) use hosts::{enabled_host, launcher_program};
pub use labelled::labelled_group;
pub use naming::PlaceName;
pub(crate) use opening::show_opened;
pub use opening::{
    chat_place_env, open_in_background, place_env, shoal_command, show_place_elsewhere,
};
pub use placement::Placement;
pub(crate) use prompts::agents_can_be_prompted;
pub use prompts::{PromptSession, WAKE_PROMPT, send_keys, submitted_by_shoal};
pub(crate) use recorded::shown_elsewhere;
pub use recorded::{
    Recorded, attach, known_sessions, left_open, open_session_owners, report_detach,
    session_owners, sessions, show_elsewhere, shown_here,
};
pub(crate) use sessions::OwnerKind;
pub use sessions::{Owner, Session, chat_session_name};
pub use typed::run as run_launch;
