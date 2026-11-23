//! Alerts an agent sends from its chat: the turn it marks sends its final
//! message, once it ends, to the recipients `shoal send --in-chat` named,
//! through the operation that sends every alert. Such an alert to the user
//! is read once the user prompts the agent in that chat.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Context as _, Result, bail};
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::executions::ExecutionKind;
use shoal_protocol::{
    alerts::{Recipient, Sent},
    chat_marks::{ChatMarked, MarkedRecipients},
    files::NewFile,
    hub::routing::Queued,
    model::Located,
    protocol::AlertTarget,
    space_names::SpaceSelector,
};

use super::Mark;
use crate::{
    model::Space,
    repository::Selector,
    {
        alerts::{Recipients, recipient},
        notifications::Source,
        scope::{Caller, ScopedExecution},
    },
};

/// The alert a turn's mark sends once the turn ends, as its record keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct MarkedAlert {
    /// Where it goes, a space by its ID.
    pub(super) to: AlertTarget,
    /// The files it carries, by absolute path, which Shoal copies once the
    /// turn ends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) attachments: Vec<NewFile>,
    /// Whom it reaches, as the mark showed them.
    pub(super) recipients: MarkedRecipients,
}

/// The text an alert to `to` carries for a turn's final `message`: one
/// line up to the coordinator or the user, lines down to agents, each kept
/// to the length and characters of such alerts, so it can always be sent.
fn alert_text(to: &AlertTarget, message: Option<&str>) -> String {
    let text: String = (message.unwrap_or_default().chars())
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let (text, max) = match to {
        AlertTarget::Up => (
            text.split_whitespace().collect::<Vec<_>>().join(" "),
            crate::validate::MAX_MESSAGE_LEN,
        ),
        AlertTarget::Space { .. } | AlertTarget::All { .. } => (
            text.trim().to_owned(),
            crate::validate::MAX_AGENT_MESSAGE_LEN,
        ),
    };
    match text.as_str() {
        "" => super::WROTE_IN_CHAT.to_owned(),
        _ => crate::validate::shorten(text, max),
    }
}

/// Mark read the user's unread alerts that the space `id` sent from its
/// chat, which a prompt the user submits there answers. Alerts are
/// one-way, so the prompt is no reply to them.
pub(super) fn read_in_chat(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute(
        "UPDATE alerts SET read=1 WHERE recipient_id IS NULL AND read=0 AND in_chat
            AND sender='space' AND space_id=?1",
        [id],
    )?;
    Ok(())
}

/// Record that the alerts `ids` carry a turn's final message.
fn mark_in_chat(tx: &Transaction<'_>, ids: &[i64]) -> Result<()> {
    for id in ids {
        tx.execute("UPDATE alerts SET in_chat=1 WHERE id=?1", [id])?;
    }
    Ok(())
}

impl Spaces {
    /// Mark the current turn of `caller`'s agent as sending its final
    /// message to `to` with copies of `attachments`, once it ends.
    pub async fn mark_chat_alert(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        to: AlertTarget,
        attachments: Vec<NewFile>,
    ) -> Result<ChatMarked> {
        let own = caller.context("only an agent Shoal runs writes in its chat")?;
        let space = self.space(&SpaceSelector::id(&own.space_id)).await?;
        let turn = self.marked_turn(daemon, caller, &space).await?;
        crate::workspace_files::check_sent(&attachments)?;
        let (to, recipients) = self.marked_recipients(daemon, &space, to).await?;
        refuse_files_elsewhere(daemon, &recipients, &attachments)?;
        let alert = MarkedAlert {
            to,
            attachments,
            recipients,
        };
        self.mark_turn(turn, Mark::Alert(alert)).await
    }

    /// Whom an alert from `space` to `to` reaches now, with `to` naming a
    /// space by its ID.
    async fn marked_recipients(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        to: AlertTarget,
    ) -> Result<(AlertTarget, MarkedRecipients)> {
        Ok(match to {
            AlertTarget::Up => (AlertTarget::Up, self.recipient_up(daemon, space).await?),
            AlertTarget::Space { space } => {
                let (space, machine) = self.located_recipient(daemon, &space).await?;
                let to = AlertTarget::Space {
                    space: SpaceSelector::id(&space.id),
                };
                (to, MarkedRecipients::Space { space, machine })
            }
            AlertTarget::All { repository } => {
                let name = match &repository {
                    Some(selector) => Some(self.repository_name(daemon, selector).await?),
                    None => None,
                };
                let to = AlertTarget::All { repository };
                (to, MarkedRecipients::All { repository: name })
            }
        })
    }

    /// The space `selector` names, as an alert names its recipient, with
    /// the name of its machine when that is another one.
    async fn located_recipient(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<(Recipient, Option<String>)> {
        Ok(match self.space_anywhere(daemon, selector).await? {
            Located::Here(space) => (recipient(&space), None),
            Located::Elsewhere { space, machine } => {
                let space = Recipient {
                    number: space.space.number,
                    id: space.space.id,
                    name: space.space.name,
                };
                (space, Some(machine.name))
            }
        })
    }

    /// Whom an alert sent up from `space` reaches now.
    async fn recipient_up(&self, daemon: &dyn Daemon, space: &Space) -> Result<MarkedRecipients> {
        Ok(
            match daemon
                .coordination()
                .route_up(daemon, &space.id, Source::Agent)
                .await?
            {
                Recipients::User => MarkedRecipients::User,
                Recipients::Coordinator(id) => MarkedRecipients::Coordinator {
                    space: recipient(&self.space(&SpaceSelector::id(&id)).await?),
                },
                Recipients::Above(_) => MarkedRecipients::Above,
                Recipients::Spaces(_) => bail!("an alert sent up reaches one recipient"),
            },
        )
    }

    /// The name of the repository `selector` names, as text shows it.
    async fn repository_name(&self, daemon: &dyn Daemon, selector: &Selector) -> Result<String> {
        let repository = daemon.repository(selector.clone()).await?;
        Ok(repository.display_name().to_owned())
    }

    /// Send `message`, the final message of the turn `turn` of the agent
    /// of `space`, as `alert`, from that agent, under its scope then.
    pub(super) async fn send_marked_alert(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        turn: ExecutionId,
        alert: MarkedAlert,
        message: Option<String>,
    ) -> Result<()> {
        let caller = agent_caller(space.id.clone(), turn);
        let MarkedAlert {
            mut to,
            attachments,
            recipients,
        } = alert;
        daemon
            .coordination()
            .scope_alert_target(daemon, &caller, &mut to)
            .await?;
        let text = alert_text(&to, message.as_deref());
        let sent = held_as_sent(
            daemon
                .coordination()
                .send_alert(daemon, Some(&caller), to, text, attachments)
                .await,
        )?;
        let here = recorded_here(&recipients, sent);
        self.store.write(move |tx| mark_in_chat(tx, &here)).await
    }
}

/// Refuse `attachments` to `recipients` on another machine, which cannot
/// read this machine's files.
fn refuse_files_elsewhere(
    daemon: &dyn Daemon,
    recipients: &MarkedRecipients,
    attachments: &[NewFile],
) -> Result<()> {
    match recipients {
        MarkedRecipients::Space {
            machine: Some(machine),
            ..
        } if !attachments.is_empty() => Err(daemon.files_stay_here(&format!("machine {machine}"))),
        _ => Ok(()),
    }
}

/// What `sent` sent, none while the hub holds it until the recipient's
/// machine connects.
fn held_as_sent(sent: Result<Sent>) -> Result<Option<Sent>> {
    match sent {
        Err(error) if error.is::<Queued>() => Ok(None),
        sent => sent.map(Some),
    }
}

/// The IDs of this machine's alerts in `sent`, an alert to `recipients`:
/// none for another machine's space, whose IDs name that machine's.
fn recorded_here(recipients: &MarkedRecipients, sent: Option<Sent>) -> Vec<i64> {
    match (recipients, sent) {
        (
            MarkedRecipients::Space {
                machine: Some(_), ..
            },
            _,
        )
        | (_, None) => Vec::new(),
        (_, Some(sent)) => sent.alerts,
    }
}

/// The agent of the space `space_id` that runs as `turn`, as the caller
/// its marked turn sends as.
fn agent_caller(space_id: SpaceId, turn: ExecutionId) -> Caller {
    Caller {
        execution: Some(ScopedExecution {
            id: turn,
            kind: ExecutionKind::Command,
            agent_execution: None,
        }),
        ..Caller::of_space(space_id)
    }
}
