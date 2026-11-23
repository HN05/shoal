//! The daemon's answers to [`SpaceMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    manager::Manager,
    scope::{Caller, Scoped, denied},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{self, Body, ErrorCode, Response, SpaceMethod};
use shoal_protocol::{
    events::EventCursor, model::Everywhere, scope_tokens::ScopeToken, space_names::SpaceSelector,
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: SpaceMethod,
    caller: Option<&Caller>,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        SpaceMethod::WatchSpaceEvents { after, follow } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                watch_space_events(stream, request_id, manager, after, follow, guard)
            }));
        }
        SpaceMethod::SpaceLog {
            space,
            filter,
            limit,
        } => {
            let log = manager.log(space.as_ref(), filter, limit).await;
            return Ok(Answer::pages(log, Body::LogEntries));
        }
        SpaceMethod::ListSpaces => {
            Body::Spaces(manager.spaces.listed_spaces(&**manager, caller).await?)
        }
        SpaceMethod::JudgeSpaces { acting } => {
            let offers = manager
                .spaces
                .offers_here(&**manager, caller, &acting)
                .await;
            return Ok(Answer::pages(offers, Body::SpaceOffers));
        }
        SpaceMethod::ListSpacesEverywhere { here, acting } => {
            let acting = acting.as_deref();
            let everywhere = manager
                .spaces
                .spaces_everywhere(&**manager, caller, here, acting)
                .await;
            let parts = everywhere.map(Everywhere::into_parts);
            return Ok(Answer::pages(parts, Body::SpacesEverywhere));
        }
        SpaceMethod::LocateSpace { space } => Body::LocatedSpace(Box::new(
            manager
                .spaces
                .locate_space(&**manager, caller, &space)
                .await?,
        )),
        SpaceMethod::SelectSpaces {
            spaces,
            machines,
            below,
        } => Body::LocatedSpaces(
            manager
                .spaces
                .select_spaces(&**manager, caller, &spaces, &machines, below)
                .await?,
        ),
        SpaceMethod::FindSpaces { target } => Body::Spaces(
            manager
                .coordination
                .find_visible_spaces(&**manager, &target, caller)
                .await?,
        ),
        SpaceMethod::RenameSpace { space, name } => {
            Body::RenamedSpace(manager.spaces.rename_space(&space, &name).await?)
        }
        SpaceMethod::IssueScope { space } => {
            Body::ScopeToken(manager.spaces.issue_scope(&space).await?)
        }
        SpaceMethod::RevokeScope { space, token } => {
            manager.spaces.revoke_scope(&space, token).await?;
            Body::Ok
        }
        SpaceMethod::SpaceEnvironment { space } => {
            Body::SpaceEnvironment(manager.spaces.space_environment(&**manager, &space).await?)
        }
        SpaceMethod::SpaceStatus { space } => {
            let mut status = manager.spaces.space_status(&**manager, &space).await?;
            status.machine = super::forwarded::answering_machine(manager, scope).await?;
            Body::SpaceStatus(Box::new(status))
        }
        SpaceMethod::SpaceSessions { space } => {
            Body::SpaceSessions(manager.spaces.space_sessions(&**manager, &space).await?)
        }
        SpaceMethod::EnterSpace { space } => {
            Body::EnteredSpace(Box::new(manager.spaces.entered_space(&space).await?))
        }
        SpaceMethod::FollowSession {
            space,
            session,
            from,
        } => {
            // Chosen first, so a space another machine has is followed there.
            // A client follows again, from where it read, on its own.
            let resumed = !from.is_empty();
            let following = manager
                .spaces
                .session_to_follow(&**manager, &space, session, from)
                .await?;
            // Another machine's follow is checked there as it goes, its
            // token here standing for its caller only while it starts.
            let scope = scope.filter(|token| manager.hub.vouched_caller(token).is_none());
            let scope = scope.cloned();
            // Following an agent is the user's interaction with it, as
            // attaching is.
            if !resumed {
                manager
                    .agents
                    .record_user_interaction(&**manager, caller, &space)
                    .await;
            }
            let manager = manager.clone();
            return Ok(Answer::connection(
                move |stream, request_id, guard| async move {
                    manager
                        .spaces
                        .follow_session(&*manager, stream, request_id, (following, scope), guard)
                        .await
                },
            ));
        }
        SpaceMethod::AttachSession { space, viewer } => {
            let terminal = manager.open_viewer_terminal(&space, &viewer).await?;
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                let answer = (request_id, Body::Ok);
                let until = std::future::pending();
                manager.relay_viewer_terminal(stream, answer, terminal, until, guard)
            }));
        }
    }))
}

impl Checked for SpaceMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::RenameSpace { .. }
            | Self::IssueScope { .. }
            | Self::RevokeScope { .. }
            | Self::SpaceEnvironment { .. }
            | Self::SpaceStatus { .. }
            | Self::SpaceSessions { .. }
            | Self::FollowSession { .. }
            | Self::SpaceLog { .. } => Some(TargetCheck::Space),
            Self::AttachSession { .. } => Some(TargetCheck::SessionSpace),
            Self::EnterSpace { .. } => Some(TargetCheck::DirectorySpace),
            Self::ListSpaces
            | Self::JudgeSpaces { .. }
            | Self::ListSpacesEverywhere { .. }
            | Self::FindSpaces { .. }
            | Self::LocateSpace { .. }
            | Self::SelectSpaces { .. }
            | Self::WatchSpaceEvents { .. } => None,
        }
    }
}

impl Scoped for SpaceMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // A coordinator follows the spaces below it, removed ones too.
            SpaceMethod::SpaceLog { space, .. } => {
                manager.narrow_to_log(caller, space).await?;
                None
            }
            // It finds only the spaces the caller sees.
            SpaceMethod::ListSpaces
            | SpaceMethod::JudgeSpaces { .. }
            | SpaceMethod::ListSpacesEverywhere { .. }
            | SpaceMethod::FindSpaces { .. }
            | SpaceMethod::LocateSpace { .. }
            | SpaceMethod::SelectSpaces { .. } => None,
            SpaceMethod::RenameSpace { space, .. }
            | SpaceMethod::SpaceStatus { space }
            | SpaceMethod::EnterSpace { space }
            | SpaceMethod::SpaceSessions { space }
            | SpaceMethod::FollowSession { space, .. } => Some(space),
            // A token for a space its scope reaches acts for no more than
            // the caller's own.
            SpaceMethod::IssueScope { space }
            | SpaceMethod::RevokeScope { space, .. }
            | SpaceMethod::SpaceEnvironment { space } => Some(space),
            SpaceMethod::WatchSpaceEvents { .. } => return Err(denied()),
            // As `shoal attach` is in a scoped execution.
            SpaceMethod::AttachSession { .. } => {
                anyhow::bail!("attach to sessions outside scoped executions")
            }
        })
    }
}

pub(in crate::daemon) async fn watch_space_events(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    after: Option<EventCursor>,
    follow: bool,
    startup: OperationGuard,
) -> Result<()> {
    if after.is_some_and(|cursor| cursor.id() < 0) {
        return protocol::write(
            &mut stream,
            &Response::new(
                request_id,
                Body::error(
                    ErrorCode::InvalidRequest,
                    "event cursor must be nonnegative",
                ),
            ),
        )
        .await;
    }
    // Subscribe before the first read so a concurrent commit cannot be missed.
    let mut changed = manager.store.watch_changes();
    let end = crate::daemon::events::latest_id(&manager.store).await?;
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    let _finite_request = if follow {
        drop(startup);
        None
    } else {
        Some(startup)
    };
    let (mut reader, mut writer) = stream.split();
    let mut cursor = after;
    let mut pruned = None;
    loop {
        let operation = manager.gates.operation().await;
        let (items, now_pruned) =
            (crate::daemon::events::page(&manager.store, cursor, pruned, 100)).await?;
        pruned = Some(now_pruned);
        let mut delivered = false;
        for item in items {
            if let Some(id) = item.event_id() {
                if !follow && id > end {
                    break;
                }
                delivered = true;
            }
            cursor = Some(item.cursor());
            protocol::write(
                &mut writer,
                &Response::new(request_id, Body::EventItem(item)),
            )
            .await?;
        }
        // Drain the complete backlog before waiting for another commit.
        if delivered && (follow || cursor.is_some_and(|id| id < end)) {
            continue;
        }
        if !follow {
            protocol::write(&mut writer, &Response::new(request_id, Body::Ok)).await?;
            return Ok(());
        }
        drop(operation);
        let mut closed = [0u8; 1];
        tokio::select! {
            result = changed.changed() => result?,
            _ = tokio::io::AsyncReadExt::read(&mut reader, &mut closed) => return Ok(()),
        }
    }
}
