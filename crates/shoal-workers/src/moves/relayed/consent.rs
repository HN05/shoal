//! What a move of spaces of several machines asks before any of them ends,
//! in one question.
use anyhow::Result;

use super::{Relay, Relays};
use crate::{client::connection::CommandPaths, operations::Failed, protocol::operations::Failure};
use shoal_core::transfer::{Consent, MoveDestination, MoveOrder};

/// Ask what the move `order` ends, `here` with its spaces this machine runs
/// as planned, and each of `relays`, refusing before anything ends when a
/// machine refuses its spaces, or without the caller's consent to all of
/// it; the consent each relay passes on to its machine.
pub(super) async fn ask(
    paths: &CommandPaths,
    order: &MoveOrder,
    here: Option<String>,
    relays: &Relays,
) -> Result<Vec<Option<Consent>>> {
    let mut questions: Vec<String> = here.into_iter().collect();
    let mut consents = Vec::new();
    for relay in &relays.0 {
        let asked = question(paths, order, relay).await?;
        if let Some(asked) = &asked {
            questions.push(format!("On machine {}: {asked}", relay.machine));
        }
        let asked = asked.map(Consent::Answered);
        consents.push(asked.or_else(|| order.consent.clone()));
    }
    let question = questions.join("\n");
    if !questions.is_empty() && !order.consents_to(&question) {
        return Err(Failed(Failure::Unconfirmed { question }).into());
    }
    Ok(consents)
}

/// What the machine of `relay` asks before it moves its spaces as `order`
/// says, from a move there that only checks them, refused when it refuses
/// them; none when it asks nothing, as with `--yes`, which it is given.
async fn question(
    paths: &CommandPaths,
    order: &MoveOrder,
    relay: &Relay,
) -> Result<Option<String>> {
    // An answer covers every machine's question, not this one's alone.
    let consent = (order.consent == Some(Consent::Any)).then_some(Consent::Any);
    let checking = MoveOrder {
        spaces: relay.ids(),
        destination: MoveDestination::Held,
        consent,
        check_only: true,
        ..order.clone()
    };
    let error = match super::pack_there(paths, checking).await {
        Ok(_) => return Ok(None),
        Err(error) => error,
    };
    match error.downcast::<Failed>() {
        Ok(Failed(Failure::Unconfirmed { question })) => Ok(Some(question)),
        Ok(failed) => Err(failed.into()),
        Err(error) => Err(error),
    }
}
