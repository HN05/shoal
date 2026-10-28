//! Every interactive input of the CLI, and how its choices look. A command
//! asks through these operations rather than reading the terminal or running
//! fzf itself, so every prompt and picker behaves and looks the same:
//!
//! - [`prompt`]: `[y/N]` confirmations naming what an action loses
//!   ([`confirm`]), offers to run a fix ([`offer`]), choices among a few
//!   lettered answers, and free text ([`input`]), also with a default
//!   ([`input_with_default`]).
//! - [`picker`]: an fzf picker over `(id, label)` [`Entries`] that returns
//!   the chosen id ([`pick`]), a typed value ([`pick_choice`]), an issue or
//!   PR ([`pick_item`]), or the key that chose it ([`pick_with_keys`]).
//! - [`views`]: choosing a row across views ([`pick_in_views`]), reading
//!   the next view ([`next_picker_view`]) and encoding its transition
//!   ([`PickerView::transition`]).
//! - [`several`]: choosing several items at once; Tab selects each and
//!   Ctrl-A all of them ([`pick_many`]).
//! - [`place`]: a place in an ordered list, in front of one of its items or
//!   last ([`pick_place`]).
//! - [`workspaces`]: the workspace a command acts on, named, current or
//!   picked ([`select_workspace`]), listed by what the command needs of it,
//!   or picked among running chats as well ([`pick_workspace_or_chat`]).
//! - [`rows`]: the aligned labels pickers and lists show ([`workspace_rows`],
//!   [`aligned`]).
//!
//! Every prompt requires a terminal and is canceled with Ctrl-C or Esc;
//! non-interactive callers pass explicit arguments and flags such as `--yes`
//! instead. A new kind of input or choice belongs here, built on these
//! operations, so the next command can use it too.
mod picker;
mod place;
mod prompt;
mod rows;
mod several;
mod views;
mod workspaces;

pub use picker::*;
pub use place::{Place, pick_place};
pub use prompt::*;
pub use rows::*;
pub use several::{Report, confirm_losses, failure, pick_many};
pub use views::*;
pub use workspaces::*;
