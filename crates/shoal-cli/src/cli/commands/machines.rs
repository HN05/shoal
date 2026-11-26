//! Reporting the requests the hub holds for offline machines.
use anyhow::Result;
use shoal_protocol::hub::routing::Queued;

use crate::cli::{context::Context, output::Style};

/// Report that the hub holds the command's request for the offline machine
/// of its space, which is its outcome until that machine connects.
pub(super) fn queued(ctx: &Context, queued: &Queued) -> Result<i32> {
    ctx.emit_styled(Style::Warning, &queued.to_string(), queued)?;
    Ok(0)
}

/// Report the requests the hub holds for offline machines, `queued`, each
/// once per machine with how many of `noun` it holds there.
pub(super) fn queued_each(ctx: &Context, queued: &[Queued], noun: &str) -> Result<()> {
    for (machine, count) in per_machine(queued) {
        let message = match count {
            1 => machine.to_string(),
            count => machine.holding(&format!("these {count} {noun}")),
        };
        ctx.emit_styled(Style::Warning, &message, machine)?;
    }
    Ok(())
}

fn per_machine(queued: &[Queued]) -> Vec<(&Queued, usize)> {
    let mut machines: Vec<(&Queued, usize)> = Vec::new();
    for held in queued {
        match machines
            .iter_mut()
            .find(|(seen, _)| seen.machine == held.machine)
        {
            Some((seen, count)) => {
                *count += 1;
                if held.expires_at < seen.expires_at {
                    *seen = held;
                }
            }
            None => machines.push((held, 1)),
        }
    }
    machines
}

#[cfg(test)]
mod tests;
