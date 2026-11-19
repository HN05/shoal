//! Removing a space whose tree goes on on other machines: the spaces
//! directly below it there go first, each through its own machine's
//! removal, which takes the spaces below it in turn, and one that cannot go
//! keeps the space, as a child here would. One on an offline machine
//! cannot go: a removal someone asked for leaves the hub holding its
//! removal until that machine connects. While the hub cannot be asked,
//! a removal goes by the directory it last answered with, so it never
//! takes a space whose children elsewhere it cannot reach.
use anyhow::{Result, anyhow, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        Member,
        directory::Listed,
        link::Reply,
        routing::{Delivery, Forwarded, Vouched},
    },
    model::{RefPart, SpaceKind, SpaceRef},
    protocol::{Body, Operation, RemovalMethod, Response},
    removal::UnsavedWork,
    space_names::{SpaceName, SpaceSelector},
};

use crate::daemon::{lifecycle::BelowElsewhere, manager::Manager};

impl Manager {
    /// Remove the spaces directly below the space `id` that other machines
    /// run as `below` asks, returning their names; one that cannot go, or
    /// that `below` does not take, refuses the removal of `id`. While the
    /// hub cannot be asked, none are known, and they stay.
    pub(crate) async fn remove_children_elsewhere(
        &self,
        id: &SpaceId,
        below: BelowElsewhere<'_>,
    ) -> Result<Vec<SpaceName>> {
        let mut removed = Vec::new();
        for (child, machine) in self.children_elsewhere(id).await? {
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
            self.remove_on(&machine, child_id, &below)
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
    pub(crate) async fn descendants_elsewhere(&self, id: &SpaceId) -> Result<Vec<SpaceRef>> {
        let Some((machine, directory)) = self.directory_or_known().await? else {
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
    async fn children_elsewhere(&self, id: &SpaceId) -> Result<Vec<(Listed, Member)>> {
        let Some((machine, directory)) = self.directory_or_known().await? else {
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
        });
        let request = Forwarded {
            caller: Vouched::User,
            method: serde_json::to_value(method)?,
            summary: format!("the removal of space {id} with the space above it"),
        };
        let delivery = match below.queues {
            true => Delivery::Queued,
            false => Delivery::Now,
        };
        let frames = match self.send_forwarded(machine, request, delivery).await? {
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
