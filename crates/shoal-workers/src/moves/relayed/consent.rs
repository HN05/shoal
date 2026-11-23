//! What a move of spaces of several machines asks before any of them ends:
//! what ending this machine's spaces would ask here, and what each other
//! machine asks of its own, which that machine's move only checks for, all
//! in one question, so the caller consents once to everything that ends.
use anyhow::Result;

use super::{Relay, Relays};
use crate::{client::connection::CommandPaths, operations::Failed, protocol::operations::Failure};
use shoal_core::transfer::{Consent, MoveDestination, MoveOrder};

/// Ask what the move `order` ends, `here` with its spaces this machine runs
/// as planned, and each of `relays`, refusing before anything ends without
/// the caller's consent to all of it; the consent each relay passes on to
/// its machine.
pub(super) async fn ask(
    paths: &CommandPaths,
    order: &MoveOrder,
    here: Option<String>,
    relays: &Relays,
) -> Result<Vec<Option<Consent>>> {
    // A copy ends nothing, and `--yes` consents to whatever ends.
    if order.keep || order.consent == Some(Consent::Any) {
        return Ok(vec![order.consent.clone(); relays.0.len()]);
    }
    let mut questions: Vec<String> = here.into_iter().collect();
    let mut consents = Vec::new();
    for relay in &relays.0 {
        let asked = question(paths, order, relay).await?;
        if let Some(asked) = &asked {
            questions.push(format!("On machine {}: {asked}", relay.machine));
        }
        consents.push(asked.map(Consent::Answered));
    }
    let question = questions.join("\n");
    if !questions.is_empty() && !order.consents_to(&question) {
        return Err(Failed(Failure::Unconfirmed { question }).into());
    }
    Ok(consents)
}

/// What the machine of `relay` asks before it moves its spaces as `order`
/// says, from a move there that only checks them; none when it asks
/// nothing.
async fn question(
    paths: &CommandPaths,
    order: &MoveOrder,
    relay: &Relay,
) -> Result<Option<String>> {
    let checking = MoveOrder {
        spaces: relay.ids(),
        destination: MoveDestination::Held,
        consent: None,
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
