//! Saying who made a change and which thread it is for: the agent claims
//! the changes it made and unclaims those it did not, switches the thread
//! its changes go to, and moves changes into a thread or out of one. A
//! change belongs to at most one thread, an open one its space takes part
//! in.
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    changes::{Change, ChangeAuthor, ChangeNumber},
    space_names::SpaceSelector,
    threads::{Thread, ThreadNumber},
};

use super::records;
use crate::daemon::{manager::Manager, scope::Caller};

impl Manager {
    /// The changes `numbers` names, of the space `space`, else `caller`'s,
    /// for a bare number, are its agent's, and with `thread` for it.
    pub async fn claim_changes(
        &self,
        caller: Option<&Caller>,
        space: Option<&SpaceSelector>,
        numbers: &[ChangeNumber],
        thread: Option<ThreadNumber>,
    ) -> Result<Vec<Change>> {
        let named = self.named_changes(caller, space, numbers).await?;
        let thread = match thread {
            Some(number) => Some(
                self.receiving_thread(caller, number, spaces(&named))
                    .await?
                    .id,
            ),
            None => None,
        };
        (self.store)
            .write(move |tx| {
                let ids = ids(records::named(tx, &named)?);
                records::set_author(tx, &ids, ChangeAuthor::Agent)?;
                if thread.is_some() {
                    records::set_thread(tx, &ids, thread)?;
                }
                records::of_ids(tx, &ids)
            })
            .await
    }

    /// The changes `numbers` names are someone else's, for no thread.
    pub async fn unclaim_changes(
        &self,
        caller: Option<&Caller>,
        space: Option<&SpaceSelector>,
        numbers: &[ChangeNumber],
    ) -> Result<Vec<Change>> {
        let named = self.named_changes(caller, space, numbers).await?;
        (self.store)
            .write(move |tx| {
                let ids = ids(records::named(tx, &named)?);
                records::set_author(tx, &ids, ChangeAuthor::Others)?;
                records::set_thread(tx, &ids, None)?;
                records::of_ids(tx, &ids)
            })
            .await
    }

    /// Put the changes `numbers` names into the thread `number`, out of
    /// any other.
    pub async fn add_changes(
        &self,
        caller: Option<&Caller>,
        number: ThreadNumber,
        space: Option<&SpaceSelector>,
        numbers: &[ChangeNumber],
    ) -> Result<Vec<Change>> {
        let named = self.named_changes(caller, space, numbers).await?;
        let thread = self
            .receiving_thread(caller, number, spaces(&named))
            .await?
            .id;
        (self.store)
            .write(move |tx| {
                let ids = ids(records::named(tx, &named)?);
                records::set_thread(tx, &ids, Some(thread))?;
                records::of_ids(tx, &ids)
            })
            .await
    }

    /// Take the changes `numbers` names out of the thread `number`, which
    /// each must be in.
    pub async fn remove_changes(
        &self,
        caller: Option<&Caller>,
        number: ThreadNumber,
        space: Option<&SpaceSelector>,
        numbers: &[ChangeNumber],
    ) -> Result<Vec<Change>> {
        let named = self.named_changes(caller, space, numbers).await?;
        let thread = self.readable_thread(caller, number).await?;
        let (id, label) = (thread.id, thread.number);
        (self.store)
            .write(move |tx| {
                let found = records::named(tx, &named)?;
                ensure!(
                    found.iter().all(|(_, thread)| *thread == Some(id)),
                    "a change named is not in thread {label}"
                );
                let ids = ids(found);
                records::set_thread(tx, &ids, None)?;
                records::of_ids(tx, &ids)
            })
            .await
    }

    /// From now on the changes of the space `selector`'s agent are for the
    /// thread `number`, an open one the space takes part in, or for none;
    /// the thread they are for.
    pub async fn switch_thread(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        number: Option<ThreadNumber>,
    ) -> Result<Option<Thread>> {
        let space = self.space(selector).await?.id;
        let thread = match number {
            Some(number) => Some(self.receiving_thread(caller, number, [&space]).await?),
            None => None,
        };
        let id = thread.as_ref().map(|thread| thread.id);
        (self.store)
            .write(move |tx| records::switch(tx, &space, id))
            .await?;
        Ok(thread)
    }

    /// The thread the changes of the space `selector`'s agent are for now.
    pub async fn current_thread(&self, selector: &SpaceSelector) -> Result<Option<Thread>> {
        let space = self.space(selector).await?.id;
        let Some(id) = (self.store)
            .read(move |tx| records::current_thread(tx, &space))
            .await?
        else {
            return Ok(None);
        };
        self.thread_by_id(id).await
    }

    /// The space and number of each change `numbers` names for `caller`:
    /// a bare number of `space`, else of the caller's own, and a full one
    /// of the space numbered so, each a space the caller's scope reaches.
    async fn named_changes(
        &self,
        caller: Option<&Caller>,
        space: Option<&SpaceSelector>,
        numbers: &[ChangeNumber],
    ) -> Result<Vec<(SpaceId, u32)>> {
        let own = match (space, caller) {
            (Some(selector), _) => Some(self.space(selector).await?.number),
            (None, Some(caller)) => self
                .find_space(&caller.space_id)
                .await?
                .map(|space| space.number),
            (None, None) => None,
        };
        let mut named = Vec::new();
        for number in numbers {
            let (space, number) = number.resolve(own)?;
            let space = self.space(&SpaceSelector::Number(space)).await?;
            if let Some(caller) = caller {
                ensure!(
                    self.reaches(caller, &space.id).await?,
                    "change {number} is in {}, which your scope does not reach",
                    space.name
                );
            }
            named.push((space.id, number));
        }
        Ok(named)
    }

    /// The thread `number` names for `caller`, which may receive changes of
    /// `spaces`: an open one each takes part in.
    async fn receiving_thread(
        &self,
        caller: Option<&Caller>,
        number: ThreadNumber,
        spaces: impl IntoIterator<Item = &SpaceId>,
    ) -> Result<Thread> {
        let thread = self.readable_thread(caller, number).await?;
        ensure!(thread.is_open(), "thread {} is resolved", thread.number);
        for space in spaces {
            ensure!(
                thread.has_party(space),
                "thread {} is not one the space of the changes takes part in",
                thread.number
            );
        }
        Ok(thread)
    }
}

/// The spaces of the changes `named`.
fn spaces(named: &[(SpaceId, u32)]) -> impl Iterator<Item = &SpaceId> {
    named.iter().map(|(space, _)| space)
}

/// The IDs of `found` changes, with the threads they were for.
fn ids(found: Vec<(i64, Option<i64>)>) -> Vec<i64> {
    found.into_iter().map(|(id, _)| id).collect()
}
