//! Where Shoal runs work for the caller's terminal. Commands say what they
//! launch; this module chooses whether it takes the terminal or runs in the
//! background, and the [`Host`](crate::terminal::Host) that carries that out
//! with its own primitives.
mod add_place;
mod background;
mod closing;
mod demos;
mod groups;
mod here;
mod launch;
mod naming;
mod prompts;
mod sessions;

pub(in crate::cli) use add_place::{Place, add_worker, follows_caller, host_workspace, open_shell};
pub(in crate::cli) use closing::watch as watch_place;
pub(in crate::cli) use demos::{demo_screen, open_demo, send_demo_keys, start_demo};
pub(in crate::cli) use here::{
    close_background_place_when_done, close_here_when_done, closes_with, label_current,
    record_owned_here,
};
pub(crate) use here::{current_origin, current_place};
pub(crate) use launch::{detached_start_environment, start_environment};
pub(in crate::cli) use launch::{
    ensure_background, keeps_agents_in_sessions, launcher_program, rerun_where_stopped,
    restore_queued, run_agent, run_in_background, start_add, start_chat, start_chat_in_background,
    start_queued,
};
pub(in crate::cli) use naming::{Labels, PlaceName, Retitled, issue_label, relabel_space};
pub(in crate::cli) use prompts::{
    CONTINUE_PROMPT, agents_can_be_prompted, send_keys, submit_prompt, submitted_by_shoal,
};
pub(crate) use prompts::{PromptSession, WAKE_PROMPT};
pub(crate) use sessions::Attachment;
pub(in crate::cli) use sessions::{
    Recorded, attach, attachable, open_session_owners, session_owners, sessions,
};
