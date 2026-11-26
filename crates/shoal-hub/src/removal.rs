//! Removing a space whose tree goes on on other machines.
use anyhow::{Result, anyhow, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        Member,
        directory::Listed,
        link::Reply,
        routing::{Delivery, Forwarded, Vouched},
    },
    model::{EventCause, RefPart, SpaceKind, SpaceRef},
    protocol::{Body, Operation, RemovalMethod, Response},
    removal::UnsavedWork,
    space_names::{SpaceName, SpaceSelector},
};

use crate::{Hub, daemon::Daemon};

/// What a removal asks of the spaces directly below its space that other
/// machines run, each of which its own machine removes as it removes a
/// child of a space it removes.
pub struct BelowElsewhere<'a> {
    /// Leave the spaces the user holds, as a workspace process's removal
    /// does.
    pub refuse_holds: bool,
    /// The IDs of the only spaces below it the removal takes, when the
    /// caller confirmed those it saw.
    pub confirmed: Option<&'a [SpaceId]>,
    /// The cause of the removal this one is part of, which the removal of
    /// each child there takes, and which has the hub hold a child's
    /// removal while its machine is offline for a removal someone asked
    /// for; an unattended one tries again on its next round instead, so its
    /// rounds pile up no removals.
    pub cause: EventCause,
}

impl Hub {
    /// Remove the spaces directly below the space `id` that other machines
    /// run as `below` asks, returning their names; one that cannot go, or
    /// that `below` does not take, refuses the removal of `id`. While the
    /// hub cannot be asked, none are known, and they stay.
    pub async fn remove_children_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        below: BelowElsewhere<'_>,
    ) -> Result<Vec<SpaceName>> {
        let mut removed = Vec::new();
        for (child, machine) in self.children_elsewhere(daemon, id).await? {
            let (name, child_id) = (&child.space.name, &child.space.id);
            if below
                .confirmed
                .is_some_and(|confirmed| !confirmed.contains(child_id))
            {
                bail!(
                    "{name} ({child_id}), below it on machine {}, was not confirmed; remove it first",
                    machine.name
                );
            }
            self.remove_on(daemon, &machine, child_id, &below)
                .await
                .map_err(|error| {
                    anyhow!(
                        "{name} ({child_id}) on machine {} cannot go: {error:#}",
                        machine.name
                    )
                })?;
            removed.push(name.clone());
        }
        Ok(removed)
    }

    /// The spaces below the space `id` on other machines, as a removal
    /// shows those it takes, deepest first.
    pub async fn descendants_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Vec<SpaceRef>> {
        let Some((machine, directory)) = self.directory_or_known(daemon).await? else {
            return Ok(Vec::new());
        };
        let mut found: Vec<&Listed> = Vec::new();
        let mut above = vec![id.clone()];
        while let Some(parent) = above.pop() {
            for listed in &directory.spaces {
                let below = listed.space.parent.as_ref() == Some(&parent)
                    && !found.iter().any(|known| known.space.id == listed.space.id);
                if below {
                    above.push(listed.space.id.clone());
                    found.push(listed);
                }
            }
        }
        let elsewhere = found.into_iter().filter(|listed| listed.machine != machine);
        Ok(elsewhere.rev().map(space_ref).collect())
    }

    /// The spaces directly below the space `id` that other machines run,
    /// each with its machine, as the hub lists them, or as it last did
    /// while it cannot be asked.
    async fn children_elsewhere(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
    ) -> Result<Vec<(Listed, Member)>> {
        let Some((machine, directory)) = self.directory_or_known(daemon).await? else {
            return Ok(Vec::new());
        };
        Ok((directory.spaces.iter())
            .filter(|listed| listed.space.parent.as_ref() == Some(id) && listed.machine != machine)
            .filter_map(|listed| {
                let owner = directory.machine(&listed.machine)?.clone();
                Some((listed.clone(), owner))
            })
            .collect())
    }

    /// Remove the space `id` on `machine`, as the user, as `below` asks:
    /// with its unsaved files once its checkpoint keeps them, the spaces
    /// below it that the removal confirmed, and the user's holds where it
    /// leaves them.
    async fn remove_on(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        id: &SpaceId,
        below: &BelowElsewhere<'_>,
    ) -> Result<()> {
        // Its unsaved files go only once its checkpoint keeps them, as a
        // child's here do.
        let unsaved = UnsavedWork::Confirmed {
            files: true,
            detached_head: None,
            files_restorable: true,
            children: below.confirmed.map(<[SpaceId]>::to_vec),
        };
        let method = Operation::Removal(RemovalMethod::RemoveSpace {
            space: SpaceSelector::id(id),
            unsaved,
            keep_user_holds: below.refuse_holds,
            above: Some(below.cause),
        });
        let request = Forwarded {
            caller: Vouched::User,
            method: serde_json::to_value(method)?,
            summary: format!("the removal of space {id} with the space above it"),
            space: Some(id.clone()),
            unheard: false,
        };
        let delivery = match below.cause {
            EventCause::Manual => Delivery::Queued,
            _ => Delivery::Now,
        };
        let frames = match self
            .send_forwarded(daemon, machine, request, delivery)
            .await?
        {
            Reply::Answered(answered) => answered.frames,
            Reply::Queued(_) => bail!(
                "machine {} is offline, so the hub holds its removal until it connects",
                machine.name
            ),
            reply => bail!("the hub answered with an unexpected {reply:?}"),
        };
        for frame in frames {
            let response: Response = serde_json::from_value(frame)?;
            if let Body::Error { message, .. } = response.body {
                bail!("{message}");
            }
        }
        Ok(())
    }
}

/// How a removal names `listed`, a space on another machine.
fn space_ref(listed: &Listed) -> SpaceRef {
    let part = match listed.space.kind {
        SpaceKind::Workspace => RefPart::Workspace { branch: None },
        SpaceKind::Chat => RefPart::Chat,
    };
    SpaceRef {
        id: listed.space.id.clone(),
        name: listed.space.name.clone(),
        part,
    }
}
