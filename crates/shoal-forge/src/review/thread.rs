//! Review threads on a PR: an inline comment and the replies to it, and
//! whether the forge marks them resolved.
use anyhow::{Context, Result, ensure};

use crate::{backends::Reviews, transport::account::Account, transport::api::Request};
use shoal_forge_types::{
    ForgeRepo,
    items::action::ThreadChange,
    numbers::{CommentId, PullNumber},
    review::thread::Thread,
};

/// Reading and acting on review threads.
#[shoal_forge_derive::extension(pub ThreadRequests, pub(crate) ThreadSteps)]
impl ForgeRepo {
    /// The thread of `comment` on PR `number`.
    pub async fn thread(
        &self,
        account: &Account,
        number: PullNumber,
        comment: CommentId,
    ) -> Result<Thread> {
        let thread = self.kind().thread(self, account, number, comment).await?;
        thread.with_context(|| format!("comment {comment} is not in a review thread of #{number}"))
    }

    /// The requests that reply in `thread` and then change its resolution.
    /// Refuses before any request when the resolution cannot change.
    pub(crate) fn plan_thread(
        &self,
        number: PullNumber,
        thread: &Thread,
        change: &ThreadChange,
    ) -> Result<Vec<Request>> {
        let resolution = match change.resolve {
            Some(resolved) => {
                ensure!(
                    thread.resolved != Some(resolved),
                    "thread {} is already {}",
                    thread.first,
                    if resolved { "resolved" } else { "unresolved" }
                );
                Some(self.kind().resolution(self, number, thread, resolved)?)
            }
            None => None,
        };
        let reply = change
            .reply
            .as_deref()
            .map(|body| self.kind().reply(self, number, thread, body))
            .transpose()?;
        Ok(reply.into_iter().chain(resolution).collect())
    }
}

#[cfg(test)]
mod tests;
