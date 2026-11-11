//! The terminal hosts Shoal runs work in: the [`Host`] trait, the
//! capability traits in [`host`] a host may support, the registry that
//! chooses one under config, and the hosts themselves. Generic code never
//! names a host; adding one means naming it in
//! [`HostName`](crate::sessions::HostName), implementing [`Host`] and the
//! capability traits it supports, and registering it where the compiler then
//! asks; a `default_host` value of its name makes it the host for terminals
//! no host serves.
mod environment;
mod herdr;
mod here;
mod host;
pub mod hosts;
mod naming;
mod prompts;
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
    Attaches, Demo, DemoWorker, Host, NOT_INHERITED_BY_DEMOS, PlaceState, Places, Sessions, Start,
    Watched, Watches,
};
pub(crate) use hosts::launcher_program;
pub use hosts::{
    Beside, Configured, Options, background_place, current, enabled_host, hosts, installed_hosts,
    keeps_agents_in_sessions, named, names_places,
};
pub use naming::{PlaceName, issue_label};
pub(crate) use prompts::{PromptSession, agents_can_be_prompted};
pub use prompts::{WAKE_PROMPT, send_keys, submit_prompt};
pub(crate) use sessions::{Attachment, OwnerKind};
pub use sessions::{Owner, Session};
pub use typed::run as run_launch;
