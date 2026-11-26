//! Every interactive input of the CLI, and how its choices look. A command
//! asks through these operations rather than reading the terminal or running
//! fzf itself, so every prompt and picker behaves and looks the same:
//!
//! - [`prompt`]: `[y/N]` confirmations naming what an action loses
//!   ([`confirm`]), offers to run a fix ([`offer`]), choices among a few
//!   lettered answers, and free text ([`input`]), also with a default
//!   ([`input_with_default`]) or asked again until a check accepts it
//!   ([`input_until`]).
//! - [`picker`]: an fzf picker over `(id, label)` [`Entries`] that returns
//!   the chosen id ([`pick`]), also under a header naming what cannot be
//!   picked ([`pick_with_header`]), a typed value ([`pick_choice`]), an issue or
//!   PR ([`pick_item`]), a CI run ([`pick_run`]), or the key that chose it
//!   ([`pick_with_keys`]), or whose keys act on an entry without leaving it
//!   ([`pick_in_place`]);
//!   [`can_pick`] tells whether one can run, for choices with a usual answer.
//! - [`keys`]: the keys pickers bind ([`Key`]), which only this module
//!   spells as fzf names.
//! - [`answers`]: an answer to a thread post's surface, one of its actions
//!   with the values it takes, or a reply in words ([`pick_answer`]).
//! - [`linked`]: one of the issues and PRs a space links, to view or open in
//!   the browser ([`pick_linked`]).
//! - [`modes`]: the mode a start takes, named or picked among the modes
//!   of the kind of space it starts in ([`ModeChoice`]).
//! - [`views`]: choosing a row across views ([`pick_in_views`]), reading
//!   the next view ([`next_picker_view`]) and encoding its transition
//!   ([`PickerView::transition`]).
//! - [`several`]: choosing several items at once; Tab selects each and
//!   Ctrl-A all of them ([`pick_many`]), also starting from a selection
//!   ([`pick_many_selected`]).
//! - [`workspaces`]: the workspace a command acts on, named, current or
//!   picked ([`select_workspace`]), listed by what the command needs of it.
//! - [`spaces`]: the space a command acts on, a workspace or a chat, picked
//!   among running chats as well as workspaces ([`select_space`],
//!   [`pick_space`]), several spaces at once ([`select_spaces`]), or those
//!   `shoal resume` brings back ([`select_resumable`]).
//! - [`elsewhere`]: the other joined machines' spaces those pickers offer
//!   after this machine's, for a command that acts on them.
//! - [`machines`]: another joined machine, picked among those online
//!   ([`pick_machine`]) where there is one ([`can_pick_machine`]).
//! - [`rows`]: the aligned labels pickers and lists show ([`workspace_rows`],
//!   [`aligned`]).
//!
//! Every prompt requires a terminal and is canceled with Ctrl-C or Esc;
//! non-interactive callers pass explicit arguments and flags such as `--yes`
//! instead, and are refused naming what the prompt asks for, or the option a
//! caller names with [`require_argument`], or one of several with
//! [`require_one_of`]. A new kind of input or choice
//! belongs here, built on these operations, so the next command can use it too.
mod acting;
mod answers;
mod elsewhere;
mod fzf;
mod keys;
mod linked;
mod machines;
mod modes;
mod offers;
mod picker;
mod prompt;
mod rows;
mod several;
mod spaces;
mod views;
mod workspaces;

pub use acting::Acting;
pub use answers::{Answering, pick_answer};
use elsewhere::elsewhere_entries;
pub use keys::Key;
pub use linked::pick_linked;
pub use machines::{can_pick_machine, pick_machine};
pub use modes::ModeChoice;
pub use picker::*;
pub use prompt::*;
pub use rows::*;
pub use several::{Report, confirm_losses, failure, pick_many, pick_many_selected};
pub use spaces::*;
pub use views::*;
pub use workspaces::*;
