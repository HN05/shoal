//! `shoal pr react` and `shoal issue react`.
use crate::{
    cli::args::changes::ReactArgs,
    forge::items::{action::Action, reaction::ReactionChange},
};

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
