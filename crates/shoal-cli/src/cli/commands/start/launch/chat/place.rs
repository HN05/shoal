//! The place a starter opens for the chat it recorded, beside its terminal
//! or in the background, which runs that chat's agent: the starter passes
//! it the chat with `--chat`, waits for a background place to report that
//! the agent started, and hands a scoped starter's place a scope issued for
//! it, as the daemon's chat launch does.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result};

use super::{Launch, command};
use crate::{
    chat_handoff::Handoff,
    cli::{context::Context, terminal},
    model::Chat,
};
use shoal_protocol::chats::LaunchedChat;
use shoal_protocol::launches::{Launched, Started};
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

impl Launch {
    /// Hand the chat `launched` recorded to a new background place, which
    /// runs its agent there, and wait until the place reports that it does.
    pub(super) async fn hand_to_background(
        self,
        ctx: &Context,
        launched: LaunchedChat,
    ) -> Result<Launched> {
        let LaunchedChat { chat, warnings } = launched;
        let scope = PlaceScope::issue(ctx, &chat).await?;
        let place = self.open_in_background(ctx, &chat, scope.as_ref()).await;
        if let Some(scope) = scope {
            scope.revoke(ctx).await;
        }
        Ok(Launched {
            space: Box::new(chat.into()),
            entry: None,
            warnings,
            start: Started::Background { place: place? },
        })
    }

    /// Open a background place that runs the recorded `chat`'s agent with
    /// `scope`, and wait until it reports that it does, returning where it
    /// runs.
    async fn open_in_background(
        &self,
        ctx: &Context,
        chat: &Chat,
        scope: Option<&PlaceScope>,
    ) -> Result<String> {
        // Opening the place does not start the chat, so the start waits for
        // its report.
        let handoff = Handoff::create(scope.map(|scope| &scope.token))?;
        let relaunch = self.relaunch(chat, Some(handoff.path()));
        let relaunch = crate::terminal::shoal_command(&ctx.paths, &relaunch)?;
        let launch = &self.chat;
        let place = shoal_workers::places::queued::start_chat(
            &ctx.paths,
            &launch.settings,
            launch.repository.registered(),
            launch.home().path(),
            &chat.name,
            relaunch,
            false,
        )
        .await?;
        handoff.wait().await?;
        Ok(place)
    }

    /// Run the recorded `chat`'s agent in a new tab or session beside this
    /// terminal, in its checkout or worktree, focused or in the background
    /// as its mode says. A background start waits for its place to report,
    /// as [`Launch::hand_to_background`] does, since nothing shows it here.
    pub(super) async fn start_beside(&self, ctx: &Context, chat: &Chat) -> Result<()> {
        let launch = &self.chat;
        let waits_for_user = command::waits_for_user(launch, &self.options);
        let placement =
            terminal::Placement::of(&launch.settings, &launch.mode, None, waits_for_user);
        let handoff = match placement {
            terminal::Placement::Background => Some(Handoff::create(None)?),
            terminal::Placement::Focus => None,
        };
        let relaunch = self.relaunch(chat, handoff.as_ref().map(Handoff::path));
        let home = (launch.repository.registered(), launch.home().path());
        let started =
            terminal::start_chat(ctx, &launch.settings, home, chat, placement, &relaunch).await?;
        if let terminal::ChatStarted::Background(report) = started {
            if let Some(handoff) = handoff {
                handoff.wait().await?;
            }
            if !ctx.json {
                outln!("{report}");
            }
        }
        Ok(())
    }

    /// The `shoal` arguments that run the recorded `chat`'s agent in the
    /// place where it opens, reporting to the `handoff` of the starter that
    /// waits on it.
    fn relaunch(&self, chat: &Chat, handoff: Option<&Path>) -> Vec<OsString> {
        let options = (self.options).in_new_place((&self.agent, &chat.id), &chat.name, handoff);
        self.topic.relaunch.command_line(options)
    }
}

/// The scope a scoped starter hands the place of a chat it recorded: a
/// token issued for the space the chat went below, with which the place
/// takes the chat and acts in the chat's own scope, revoked once the place
/// reports.
struct PlaceScope {
    space: SpaceSelector,
    token: ScopeToken,
}

impl PlaceScope {
    /// The scope this process hands the place of the recorded `chat`, which
    /// goes below a space its scope reaches; none when it is unscoped.
    async fn issue(ctx: &Context, chat: &Chat) -> Result<Option<Self>> {
        if !crate::env::is_scoped() {
            return Ok(None);
        }
        let parent = (chat.parent.as_ref()).context("a scoped process's chat has no parent")?;
        let space = SpaceSelector::id(&parent.id);
        let token = crate::client::spaces::issue_scope(&ctx.paths, space.clone()).await?;
        Ok(Some(Self { space, token }))
    }

    /// Revoke this scope once the place no longer needs it. A token it
    /// cannot revoke goes with the space it was issued for.
    async fn revoke(self, ctx: &Context) {
        let _ = crate::client::spaces::revoke_scope(&ctx.paths, self.space, self.token).await;
    }
}
