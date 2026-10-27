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
//!   PR ([`pick_item`]), the key that chose it ([`pick_with_keys`]), or a row
//!   of one of several views ([`pick_in_views`]).
//! - [`several`]: choosing several items at once; Tab selects each and
//!   Ctrl-A all of them ([`pick_many`]).
//! - [`workspaces`]: the workspace a command acts on, named, current or
//!   picked ([`select_workspace`]), listed by what the command needs of it.
//! - [`rows`]: the aligned labels pickers and lists show ([`workspace_rows`],
//!   [`aligned`]).
//!
//! Every prompt requires a terminal and is canceled with Ctrl-C or Esc;
//! non-interactive callers pass explicit arguments and flags such as `--yes`
//! instead. A new kind of input or choice belongs here, built on these
//! operations, so the next command can use it too.
mod picker;
mod prompt;
mod rows;
mod several;
mod workspaces;

pub use picker::*;
pub use prompt::*;
pub use rows::*;
pub use several::{Report, confirm_losses, failure, pick_many};
pub use workspaces::*;
