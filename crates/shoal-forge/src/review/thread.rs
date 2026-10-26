//! Review threads on a PR: an inline comment and the replies to it, and
//! whether the forge marks them resolved.
use anyhow::{Context, Result, ensure};

use crate::{
    ForgeRepo, account::Account, backends::Backend, items::action::ThreadChange,
    transport::api::Request,
};

/// A review thread, found from any of its comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// The comment that starts the thread.
    pub first: u64,
    /// The comments after the first.
    pub replies: usize,
    /// Unknown where the forge does not report it.
    pub resolved: Option<bool>,
    pub(crate) target: Target,
}

/// What a forge's reply and resolution requests name besides the first comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// The GraphQL node its resolution mutations take.
    GitHub {
        node: String,
    },
    GitLab {
        discussion: String,
    },
    Bitbucket,
    /// A reply is a comment of the thread's review on the first comment's
    /// lines: `line` starts them, negative on the old side.
    Forgejo {
        review: u64,
        path: String,
        line: i64,
        extra_lines: i64,
    },
}

impl ForgeRepo {
    /// The thread of `comment` on PR `number`.
    pub async fn thread(&self, account: &Account, number: u64, comment: u64) -> Result<Thread> {
        let thread = self.kind.thread(self, account, number, comment).await?;
        thread.with_context(|| format!("comment {comment} is not in a review thread of #{number}"))
    }

    /// The requests that reply in `thread` and then change its resolution.
    /// Refuses before any request when the resolution cannot change.
    pub(crate) fn plan_thread(
        &self,
        number: u64,
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
                Some(self.kind.resolution(self, number, thread, resolved)?)
            }
            None => None,
        };
        let reply = change
            .reply
            .as_deref()
            .map(|body| self.kind.reply(self, number, thread, body))
            .transpose()?;
        Ok(reply.into_iter().chain(resolution).collect())
    }
}

#[cfg(test)]
mod tests;
