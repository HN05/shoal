//! `shoal pr react` and `shoal issue react`: a reaction to add or remove.
use crate::{
    cli::args::changes::ReactArgs,
    forge::items::{action::Action, reaction::ReactionChange},
};

/// The reaction change `args` describe, and what to report once it is made.
pub(super) fn react(args: ReactArgs) -> (Action, &'static str) {
    let outcome = if args.remove {
        "Reaction removed"
    } else {
        "Reaction added"
    };
    let change = ReactionChange {
        comment: args.comment,
        content: args.reaction,
        remove: args.remove,
    };
    (Action::React(change), outcome)
}
