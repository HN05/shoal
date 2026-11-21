//! Sending an alert from the user or a space, up to a coordinator or the
//! user, or down to the agents of spaces, and withdrawing an unread one.
use anyhow::{Context as _, Result, bail, ensure};
use rusqlite::{OptionalExtension as _, Transaction, params};

use super::{About, Alert, COLUMNS, FROM, NewAlert, Reader, Recipients, Route, Sender, Sent};
use crate::{
    daemon::{
        manager::Manager,
        scope::Caller,
        workspace_files::{FileSource, NewFile, SentFile},
    },
    model::{Space, SpaceKind},
    repository::Selector,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    alerts::{AlertKind, Reached, ReachedSpaces, Recipient, Refused, RefusedSpace},
    hub::Member,
    model::Located,
    protocol::{AlertMethod, AlertTarget, Operation},
    space_names::SpaceSelector,
};

/// An alert to another machine's space `space`, which `machine` runs.
struct AlertElsewhere<'a> {
    space: &'a SpaceId,
    machine: &'a Member,
    message: String,
    attachments: Vec<NewFile>,
}

impl Manager {
    /// Send `message` with `attachments` from `caller`, or from the user
    /// without one, to `to`; where it went.
    pub async fn send_alert(
        &self,
        caller: Option<&Caller>,
        to: AlertTarget,
        message: String,
        attachments: Vec<NewFile>,
    ) -> Result<Sent> {
        match to {
            AlertTarget::Up => {
                let caller =
                    caller.context("the user has no space above; name a space to send to")?;
                self.send_up(caller, message, attachments).await
            }
            AlertTarget::Space { space } => {
                (self.send_space_message(caller, &space, message, attachments)).await
            }
            AlertTarget::All { repository } => {
                let sent = self.send_all_message(caller, repository.as_ref(), message, attachments);
                sent.await
            }
        }
    }

    /// Send `message` from `caller`'s space up to its coordinator, or else
    /// the user, with copies of the files `attachments`, which the space's
    /// attached files keep for as long as its results; it may be empty
    /// when the files are the message. A message to the user is one line,
    /// since it may show as a terminal notification.
    async fn send_up(
        &self,
        caller: &Caller,
        message: String,
        attachments: Vec<NewFile>,
    ) -> Result<Sent> {
        if !message.is_empty() || attachments.is_empty() {
            crate::validate::message(&message)?;
        }
        crate::daemon::workspace_files::check_sent(&attachments)?;
        let space = self.space(&SpaceSelector::id(&caller.space_id)).await?;
        let about = About::space(&space.name, &space.id);
        let (id, text) = (space.id.clone(), message.clone());
        let attacher = self.attacher(Some(caller)).await;
        let sent = (self.send_with_files(
            &space.into(),
            FileSource::Alert,
            &text,
            attachments,
            &attacher,
            |files| async move { self.record_sent(sent_alert(about, message, &files)).await },
        ))
        .await?;
        crate::daemon::events::record_alert(&self.store, &id, text, sent.alerts.clone()).await;
        Ok(sent)
    }

    /// Send `message` from `caller` to the agent of the space `selector`
    /// names, which receives it now, once its agent next runs, or once one
    /// starts there.
    pub async fn send_space_message(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        message: String,
        attachments: Vec<NewFile>,
    ) -> Result<Sent> {
        // Its sender's reach was checked, and it may be a space of another
        // machine that forwarded the alert here.
        if let Located::Elsewhere { space, machine } = self.space_anywhere(selector).await? {
            let alert = AlertElsewhere {
                space: &space.space.id,
                machine: &machine,
                message,
                attachments,
            };
            return self.send_elsewhere(caller, alert).await;
        }
        let route = Route::Space(selector);
        self.send_down(caller, route, message, attachments, true)
            .await
    }

    /// Send `alert` from `caller` to its space, which another machine runs,
    /// as a request to send it there would: through the hub, which holds it
    /// while that machine is offline, when it reached no one yet.
    async fn send_elsewhere(
        &self,
        caller: Option<&Caller>,
        alert: AlertElsewhere<'_>,
    ) -> Result<Sent> {
        let operation = Operation::Alert(AlertMethod::SendAlert {
            to: AlertTarget::Space {
                space: SpaceSelector::id(alert.space),
            },
            from: None,
            message: alert.message,
            attachments: alert.attachments,
        });
        let answer = self
            .hub
            .ask_machine_or_hold(self, alert.machine, operation, caller);
        Sent::try_from(answer.await?)
    }

    /// Send `message` from `caller` to every workspace and chat with an
    /// agent, running, reattaching, stopped, queued or waiting for its
    /// restore, that its scope reaches but its own space, of `repository`
    /// when it names one. An agent that does not run reads the message
    /// once it next runs, which the message does not cause, except for a
    /// restore waiting for an update. A space that refuses the message,
    /// such as one whose unread alerts are full or that removal took
    /// meanwhile, is listed with why, and the others still receive it.
    /// Identical unread alerts collapse, so sending again does not repeat it
    /// to recipients that have not read it yet.
    pub async fn send_all_message(
        &self,
        caller: Option<&Caller>,
        repository: Option<&Selector>,
        message: String,
        attachments: Vec<NewFile>,
    ) -> Result<Sent> {
        let route = Route::All { caller, repository };
        self.send_down(caller, route, message, attachments, false)
            .await
    }

    /// Send `message` from `caller` to each space `route` reaches, which
    /// the log of the caller's space records; a refusal fails the whole
    /// send when `refusals_fail`, and otherwise lists the space that
    /// refused it.
    async fn send_down(
        &self,
        caller: Option<&Caller>,
        route: Route<'_>,
        message: String,
        attachments: Vec<NewFile>,
        refusals_fail: bool,
    ) -> Result<Sent> {
        let Recipients::Spaces(spaces) = self.route(route).await? else {
            bail!("an alert sent down reaches spaces");
        };
        let (attacher, sender) = (self.attacher(caller).await, self.sender(caller).await?);
        let mut sent = Sent::default();
        for (space, reader) in spaces {
            let (text, files) = (message.clone(), attachments.clone());
            let queued = match self
                .send_to(space.clone(), &sender, text, files, &attacher)
                .await
            {
                Err(error) if refusals_fail => return Err(error),
                queued => queued,
            };
            if let Ok(id) = queued {
                sent.alerts.push(id);
            }
            outcome(&mut sent.reached, reader, queued.map(drop)).space(&space);
        }
        if let Some(caller) = caller
            && !sent.alerts.is_empty()
        {
            let (id, alerts) = (&caller.space_id, sent.alerts.clone());
            crate::daemon::events::record_alert(&self.store, id, message, alerts).await;
        }
        Ok(sent)
    }

    /// Narrow `to`, where the scoped `caller` sends an alert, to what its
    /// scope reaches: a space below its own, here or on another machine,
    /// by its ID, or the agents below it of its own repository.
    pub(in crate::daemon) async fn scope_alert_target(
        &self,
        caller: &Caller,
        to: &mut AlertTarget,
    ) -> Result<()> {
        const REFUSAL: &str = "scoped processes send alerts down only to the spaces below theirs";
        match to {
            AlertTarget::Space { space } => {
                let id = self.reached_space(caller, space).await?;
                ensure!(id != caller.space_id, "{REFUSAL}");
                *space = SpaceSelector::id(&id);
            }
            AlertTarget::All {
                repository: Some(repository),
            } => {
                *repository = self.own_repository(caller, repository, REFUSAL).await?;
            }
            AlertTarget::Up | AlertTarget::All { repository: None } => {}
        }
        Ok(())
    }

    /// Who an alert from `caller` comes from: its space, which it names,
    /// else the user. A caller another machine vouches for names its space
    /// by its ID alone, since this machine does not keep that space.
    pub(crate) async fn sender(&self, caller: Option<&Caller>) -> Result<(Sender, About)> {
        let Some(caller) = caller else {
            return Ok((Sender::User, About::default()));
        };
        let about = match self.find_space(&caller.space_id).await? {
            Some(space) => About::space(&space.name, &space.id),
            None => About {
                id: Some(caller.space_id.clone()),
                ..About::default()
            },
        };
        Ok((Sender::Space, about))
    }

    /// Delete the unread alert `id` that `caller`, or the user without one,
    /// sent, to the space `recipient` names when it names one, which its
    /// recipient need not read after all. A read alert stays; a correction
    /// follows it. A recipient another machine has refuses it here, which
    /// sends the request on to that machine, where the alert is.
    pub async fn withdraw_alert(
        &self,
        caller: Option<&Caller>,
        recipient: Option<&SpaceSelector>,
        id: i64,
    ) -> Result<Alert> {
        let recipient = match recipient {
            Some(recipient) => Some(self.space(recipient).await?.id),
            None => None,
        };
        let sender = caller.map(|caller| caller.space_id.clone());
        let withdrawn = Withdrawn {
            id,
            sender,
            recipient,
        };
        self.store.write(move |tx| withdrawn.delete(tx)).await
    }
}

/// The message `message` the space `about` sends with Shoal's copies
/// `files`.
pub(crate) fn sent_alert(about: About, message: String, files: &[SentFile]) -> NewAlert {
    NewAlert {
        kind: AlertKind::Message,
        sender: Sender::Space,
        about,
        message,
        attachments: files.iter().map(|file| file.path.clone()).collect(),
    }
}

/// The alert `id` that the space `sender` sent, or the user without one,
/// to the space `recipient` when there is one.
struct Withdrawn {
    id: i64,
    sender: Option<SpaceId>,
    recipient: Option<SpaceId>,
}

impl Withdrawn {
    /// Delete the alert while it is unread.
    fn delete(self, tx: &Transaction<'_>) -> Result<Alert> {
        let Self {
            id,
            sender: space,
            recipient,
        } = self;
        let sender = match space {
            Some(_) => Sender::Space,
            None => Sender::User,
        };
        let alert = tx
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM {FROM} WHERE a.id=?1 AND a.sender=?2 \
                     AND a.space_id IS ?3 AND (?4 IS NULL OR a.recipient_id=?4)"
                ),
                params![id, sender, space, recipient],
                super::row,
            )
            .optional()?
            .with_context(|| format!("you sent no alert {id}"))?;
        ensure!(
            !alert.read,
            "alert {id} was already read; send a correction with shoal send"
        );
        tx.execute("DELETE FROM alerts WHERE id=?1", [id])?;
        Ok(alert)
    }
}

/// Where a space goes in `reached` once an alert to it was `sent`: among
/// those that refused it, or else by its `reader`.
fn outcome(reached: &mut Reached, reader: Reader, sent: Result<()>) -> Outcome<'_> {
    match (sent, reader) {
        (Err(error), _) => Outcome::Refused(&mut reached.refused, format!("{error:#}")),
        (Ok(()), Reader::Now) => Outcome::Reached(&mut reached.now),
        (Ok(()), Reader::OnStart) => Outcome::Reached(&mut reached.on_start),
        (Ok(()), Reader::Nobody) => Outcome::Reached(&mut reached.without_agent),
    }
}

/// The list of a [`Reached`] that a space joins.
enum Outcome<'a> {
    Reached(&'a mut ReachedSpaces),
    Refused(&'a mut Refused, String),
}

impl Outcome<'_> {
    /// Add `space` to the list among its kind's.
    fn space(self, space: &Space) {
        let recipient = recipient(space);
        match (self, space.kind()) {
            (Self::Reached(spaces), SpaceKind::Workspace) => spaces.workspaces.push(recipient),
            (Self::Reached(spaces), SpaceKind::Chat) => spaces.chats.push(recipient),
            (Self::Refused(refused, error), SpaceKind::Workspace) => {
                refused.workspaces.push(refusal(recipient, error))
            }
            (Self::Refused(refused, error), SpaceKind::Chat) => {
                refused.chats.push(refusal(recipient, error))
            }
        }
    }
}

/// `space` as an alert's result names it.
pub(crate) fn recipient(space: &Space) -> Recipient {
    Recipient {
        number: space.number,
        id: space.id.clone(),
        name: space.name.clone(),
    }
}

/// `space` refusing an alert for `error`, which the daemon logs.
fn refusal(space: Recipient, error: String) -> RefusedSpace {
    crate::daemon::log!("alert to every agent refused by {}: {error}", space.id);
    RefusedSpace { space, error }
}

#[cfg(test)]
mod tests;
