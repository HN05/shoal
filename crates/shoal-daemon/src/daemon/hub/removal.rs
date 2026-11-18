//! Removing a space whose tree goes on on other machines: the spaces
//! directly below it there go first, each through its own machine's
//! removal, which takes the spaces below it in turn, and one that cannot go
//! keeps the space, as a child here would.
use anyhow::{Result, anyhow, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        Member,
        directory::Listed,
        link::Reply,
        routing::{Delivery, Forwarded, Vouched},
    },
    protocol::{Body, Operation, RemovalMethod, Response},
    space_names::{SpaceName, SpaceSelector},
};

use crate::daemon::{
    log,
    workspace::{BelowElsewhere, Manager},
};

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
        let children = match self.children_elsewhere(id).await {
            Ok(children) => children,
            Err(error) => {
                log!("hub: could not ask which spaces below {id} other machines run: {error:#}");
                return Ok(Vec::new());
            }
        };
        let mut removed = Vec::new();
        for (child, machine) in children {
            let (name, child_id) = (&child.space.name, &child.space.id);
            if below
                .taking
                .is_some_and(|taking| !taking.contains(child_id))
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

    /// The spaces directly below the space `id` that other machines run,
    /// each with its machine.
    async fn children_elsewhere(&self, id: &SpaceId) -> Result<Vec<(Listed, Member)>> {
        let Some(view) = self.hub_view(None).await? else {
            return Ok(Vec::new());
        };
        let directory = view.directory;
        Ok((directory.spaces.iter())
            .filter(|listed| listed.space.parent.as_ref() == Some(id))
            .filter_map(|listed| {
                let machine = directory.machine(&listed.machine)?.clone();
                Some((listed.clone(), machine))
            })
            .collect())
    }

    /// Remove the space `id` on `machine`, as the user, with what `below`
    /// lets its removal delete.
    async fn remove_on(
        &self,
        machine: &Member,
        id: &SpaceId,
        below: &BelowElsewhere<'_>,
    ) -> Result<()> {
        let method = Operation::Removal(RemovalMethod::RemoveSpace {
            space: SpaceSelector::id(id),
            unsaved: below.unsaved.clone(),
        });
        let request = Forwarded {
            caller: Vouched::User,
            method: serde_json::to_value(method)?,
            summary: format!("the removal of space {id} with the space above it"),
        };
        let frames = match self.send_forwarded(machine, request, Delivery::Now).await? {
            Reply::Answered(answered) => answered.frames,
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
