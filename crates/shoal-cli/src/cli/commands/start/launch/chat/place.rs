//! The place a starter opens beside its terminal for the chat it recorded,
//! which runs that chat's agent: the starter passes it the chat with
//! `--chat` and waits for a place in the background to report that the
//! agent started.
use std::{ffi::OsString, path::Path};

use anyhow::Result;

use super::{Launch, command};
use crate::{
    chat_handoff::Handoff,
    cli::{context::Context, terminal},
    model::Chat,
};

impl Launch {
    /// Run the recorded `chat`'s agent in a new tab or session beside this
    /// terminal, in its checkout or worktree, focused or in the background
    /// as its mode says. A background start waits for its place to report,
    /// since nothing shows it here.
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
