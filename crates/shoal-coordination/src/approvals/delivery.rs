//! Delivering requests and decisions: messages to the spaces that decide or
//! made a request, and notifications for the user.
use crate::{Coordination, daemon::Daemon};
use shoal_ids::space_ids::SpaceId;

use super::{ApprovalRequest, DecisionStatus, store::give_back, text::RequestText};
use crate::{
    log,
    notifications::{About, AlertKind},
};

impl Coordination {
    /// Tell the decider of a withdrawn request that it needs no decision:
    /// the coordinating agent it was routed to, or else the user.
    pub(super) async fn announce_withdrawal(&self, daemon: &dyn Daemon, request: &ApprovalRequest) {
        let message = request.withdrawal_notification();
        if let Some(decider) = &request.decider
            && self
                .tell_space(
                    daemon,
                    decider,
                    AlertKind::ApprovalWithdrawn,
                    format!("{}: {message}", request.space_name()),
                )
                .await
        {
            return;
        }
        self.notify_about(
            daemon,
            About {
                name: request.space.clone(),
                id: Some(request.space_id.clone()),
                subject: None,
            },
            AlertKind::ApprovalWithdrawn,
            message,
        )
        .await;
    }

    /// Failure only logs; the decision stands.
    pub(super) async fn tell_requester(&self, daemon: &dyn Daemon, request: &ApprovalRequest) {
        let message = request.decision_message();
        (self.tell_space(
            daemon,
            &request.space_id,
            AlertKind::ApprovalDecided,
            message,
        ))
        .await;
    }

    /// Queue Shoal's alert of `kind` for the agent of the space `id`, here
    /// or on another joined machine; false when it cannot be, which only
    /// logs.
    pub async fn tell_space(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        kind: AlertKind,
        message: String,
    ) -> bool {
        let here = daemon
            .find_space(id)
            .await
            .is_ok_and(|space| space.is_some());
        let queued = match here || !daemon.runs_elsewhere(id).await {
            true => daemon.queue_agent_message(id, kind, message).await,
            false => daemon.message_elsewhere(id, kind, message).await,
        };
        match queued {
            Ok(()) => true,
            Err(error) => {
                log!("approval message not queued: {error:#}");
                false
            }
        }
    }

    /// Message the request's decider, or notify the user when no space
    /// decides it or its decider cannot be told.
    pub(super) async fn announce_approval(&self, daemon: &dyn Daemon, request: &ApprovalRequest) {
        let told = match &request.decider {
            Some(decider) => {
                let kind = AlertKind::ApprovalRequested;
                self.tell_space(daemon, decider, kind, request.routed_message())
                    .await
            }
            None => false,
        };
        // The user decides what no space could be told about.
        if !told {
            if request.decider.is_some() {
                self.release_to_user(request).await;
            }
            self.ask_user(daemon, request).await;
        }
    }

    /// Tell the user about requests handed to them, such as those of a chat
    /// that ended, which [`super::release_decided_by`] gave back in the same
    /// transaction that ended it.
    pub async fn ask_user_about(&self, daemon: &dyn Daemon, requests: &[ApprovalRequest]) {
        // Under the gate a withdrawal takes, with each request read again, so
        // one withdrawn meanwhile notifies nobody.
        let _gate = self.gates.lock_announcements().await;
        for request in requests {
            let stored = match self.approval_by_key(&request.key).await {
                Ok(stored) => stored,
                Err(error) => {
                    log!("approval {} not rechecked: {error:#}", request.number);
                    continue;
                }
            };
            if let Some(stored) = stored.filter(|stored| {
                stored.status == DecisionStatus::Pending && stored.decider.is_none()
            }) {
                self.ask_user(daemon, &stored).await;
            }
        }
    }

    /// Give the request back to the user when its decider cannot be told,
    /// so it is the user's to decide and their inbox lists it.
    async fn release_to_user(&self, request: &ApprovalRequest) {
        let (key, decider) = (request.key.clone(), request.decider.clone());
        let released = self
            .store
            .write(move |tx| give_back(tx, &key, decider.as_ref()))
            .await;
        if let Err(error) = released {
            log!("approval {} not given back: {error:#}", request.number);
        }
    }

    pub(super) async fn ask_user(&self, daemon: &dyn Daemon, request: &ApprovalRequest) {
        let about = About {
            name: request.space.clone(),
            id: Some(request.space_id.clone()),
            subject: Some(request.key.clone()),
        };
        self.notify_about(
            daemon,
            about,
            AlertKind::ApprovalRequested,
            request.notification(),
        )
        .await;
    }
}
