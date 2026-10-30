//! Where Shoal runs work for the caller's terminal. Commands say what they
//! launch; this module chooses whether it takes the terminal or runs in the
//! background, and the [`Host`] that carries that out with its own
//! primitives. Commands never name a host; adding one means naming it in
//! [`HostName`](crate::sessions::HostName), implementing [`Host`] and the
//! capability traits in [`host`] it supports, and registering it where the
//! compiler then asks; a `default_host` value of its name makes it the host
//! for terminals no host serves.
mod add_place;
mod background;
mod closing;
mod demos;
mod groups;
mod herdr;
mod here;
mod host;
mod hosts;
mod launch;
mod naming;
mod prompts;
mod rex;
mod sessions;
mod typed;
mod zmx;

use crate::sessions::Origin;
pub(in crate::cli) use add_place::{Place, add_worker, follows_caller, host_workspace, open_shell};
pub(in crate::cli) use closing::{PlaceState, Watched, watch as watch_place};
pub(in crate::cli) use demos::{demo_screen, open_demo, send_demo_keys, start_demo};
pub(in crate::cli) use here::{
    close_background_place_when_done, close_here_when_done, closes_with, label_current,
    record_owned_here,
};
pub(crate) use here::{current_origin, current_place};
use host::{Demo, DemoWorker, Host, Places, Start};
use hosts::{Configured, Options, names_places};
pub(crate) use launch::{detached_start_environment, start_environment};
pub(in crate::cli) use launch::{
    ensure_background, keeps_agents_in_sessions, launcher_program, rerun_where_stopped,
    restore_queued, run_agent, run_in_background, start_add, start_chat, start_queued,
};
pub(in crate::cli) use naming::{PlaceName, issue_label, relabel_chat, relabel_places};
pub(crate) use prompts::PromptSession;
pub(in crate::cli) use prompts::{agents_can_be_prompted, send_keys, submit_prompt};
pub(crate) use sessions::Attachment;
pub(in crate::cli) use sessions::{
    Owner, OwnerKind, Recorded, Session, attach, open_session_owners, session_owners, sessions,
};
pub(in crate::cli) use typed::run as run_launch;
