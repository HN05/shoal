//! The chats a package carries with its space: which go with it, which keep
//! it here, and what the package holds of each once its removal recorded it.
use anyhow::{Context, Result, ensure};

use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceName;
use crate::{
    daemon::workspace::Manager,
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

    /// What a package holds of the chats `ids` that went with its space,
    /// from the removal records their removal saved.
    pub(super) async fn carried_records(&self, ids: &[SpaceId]) -> Result<Vec<CarriedChat>> {
        let removed = self.removed_spaces().await?;
        let machine = crate::process::machine::id();
        let mut carried = Vec::new();
        for id in ids {
            let record = (removed.iter().find(|removed| &removed.space_id == id))
                .with_context(|| format!("the removal of chat {id} kept no record"))?;
            let remote_url = match &record.repository_id {
                Some(repository) => {
                    let repo = self.repository(repository).await?;
                    crate::repository::remote_url_from_source(&repo.source).await?
                }
                None => None,
            };
            carried.push(CarriedChat::of(remote_url, machine.clone(), record.clone()));
        }
        Ok(carried)
    }
}
