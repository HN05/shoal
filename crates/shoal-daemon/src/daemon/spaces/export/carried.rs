//! The chats a package carries with its space: which go with it, which keep
//! it here, and what the package holds of each once its removal recorded it.
use anyhow::{Context, Result, ensure};

use crate::client::space_ids::SpaceId;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    daemon::{
        events::EventCause,
        workspace::{Manager, StopRequest},
    },
    model::RemovedSpace,
    removal::UnsavedWork,
    transfer::{CarriedChat, CarriedChatExt},
};

impl Manager {
    /// The IDs of the chats that go with the space `id`, each after its
    /// parent among them.
    pub(super) async fn chats_carried_by(&self, id: &SpaceId) -> Result<Vec<SpaceId>> {
        Ok(crate::transfer::carried_chats(&self.chats().await?, id))
    }

    /// Refuse to move the space `id`, called `name`, while a space below it
    /// that does not go with it stays, which its removal would take.
    pub(super) async fn ensure_only_carried_below(
        &self,
        id: &SpaceId,
        name: &SpaceName,
    ) -> Result<()> {
        let carried = self.chats_carried_by(id).await?;
        let staying: Vec<String> = (self.spaces_taken_with(id).await?.into_iter())
            .filter(|space| !carried.contains(&space.id))
            .map(|space| space.name.into())
            .collect();
        ensure!(
            staying.is_empty(),
            "spaces below {name} stay here ({}); move or remove them first",
            staying.join(", ")
        );
        Ok(())
    }

    /// The IDs of the spaces below the chat `id` that its end for a move
    /// takes: those `unsaved` confirms, else the chats that go with it.
    pub(super) async fn chat_taking(
        &self,
        id: &SpaceId,
        unsaved: &UnsavedWork,
    ) -> Result<Vec<SpaceId>> {
        match unsaved.children() {
            Some(confirmed) => Ok(confirmed.to_vec()),
            None => self.chats_carried_by(id).await,
        }
    }

    /// What a package holds of the chats `ids` that went with its space,
    /// from the removal records their removal saved.
    pub(super) async fn carried_records(&self, ids: &[SpaceId]) -> Result<Vec<CarriedChat>> {
        let removed = self.removed_spaces().await?;
        let mut carried = Vec::new();
        for id in ids {
            let record = (removed.iter().find(|removed| &removed.space_id == id))
                .with_context(|| format!("the removal of chat {id} kept no record"))?;
            carried.push(self.carried(record.clone()).await?);
        }
        Ok(carried)
    }

    /// Stop the agents of the chats `ids`, saving their sessions, and say
    /// what a package holds of each, as their removal would record them,
    /// for a move that ends them only once another machine imported it.
    pub(super) async fn stopped_records(&self, ids: &[SpaceId]) -> Result<Vec<CarriedChat>> {
        let mut carried = Vec::new();
        for id in ids {
            (self.stop_space(&SpaceSelector::id(id), StopRequest::moving())).await?;
            let record = self.chat_removal_record(id, EventCause::Manual).await?;
            carried.push(self.carried(record).await?);
        }
        Ok(carried)
    }

    /// What a package holds of the chat `record` records.
    async fn carried(&self, record: RemovedSpace) -> Result<CarriedChat> {
        let remote_url = match &record.repository_id {
            Some(repository) => {
                let repo = self.repository(repository).await?;
                crate::repository::remote_url_from_source(&repo.source).await?
            }
            None => None,
        };
        let machine = crate::process::machine::id();
        Ok(CarriedChat::of(remote_url, machine, record))
    }
}
