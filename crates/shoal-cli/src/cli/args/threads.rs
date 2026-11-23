//! Arguments of `shoal thread` and of what `shoal send` takes for a post in
//! a thread.
use std::{path::PathBuf, str::FromStr};

use anyhow::{Result, ensure};
use clap::{Args, Subcommand};

use shoal_protocol::changes::ChangeNumber;
use shoal_protocol::code::Lines;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::threads::{OutcomeKind, ThreadKind, ThreadNumber, ThreadState};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ThreadCommand {
    /// Open an empty thread and print its number; post in it with shoal
    /// send <thread> "<text>".
    ///
    /// From a space, the thread goes to its coordinator, else the user, or
    /// with --space to a space below it; the user opens one with a space.
    /// Nobody hears of it until its first post.
    Open {
        /// A free-text name, such as "Which database should the cache use?"
        name: String,
        /// What the thread is for, which decides how it can end; without
        /// it, a comment of your pending review of the space, else a
        /// question.
        #[arg(long)]
        kind: Option<ThreadKind>,
        /// Space, a workspace or chat, that holds the thread.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
        /// A file, or its lines as PATH:FIRST-LAST, in a worktree your
        /// scope reaches that the thread is about; repeat for more. The
        /// thread's opening post, named as it, carries them at once.
        #[arg(long, value_name = "PATH[:FIRST-LAST]")]
        code: Vec<CodeArg>,
    },
    /// Give your role in a thread, as its opener or holder, to a space
    /// below you, or up to your coordinator or the user; the new party
    /// receives its posts from then on, and so do you unless --unlink.
    Forward {
        /// Thread number, such as 3.2, or 2 for one of your own space.
        thread: ThreadNumber,
        /// Space, a workspace or chat, to give it to; omit to pass it up.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
        /// A note for the new holder, such as why it should answer.
        #[arg(long)]
        note: Option<String>,
        /// Receive none of its later posts, which a forwarder otherwise
        /// does.
        #[arg(long)]
        unlink: bool,
    },
    /// Settle a thread with an outcome, or reopen it with --undo; its
    /// opener resolves it, or its holder one Shoal opened, such as a
    /// review's finding.
    Resolve {
        /// Thread number, such as 3.2, or 2 for one of your own space.
        thread: ThreadNumber,
        /// answered (a question), done (a request), fixed, false-positive,
        /// wont-fix or deferred (a review's finding), duplicate or
        /// withdrawn.
        #[arg(required_unless_present = "undo")]
        outcome: Option<OutcomeKind>,
        #[command(flatten)]
        data: OutcomeData,
        /// Reopen a resolved thread.
        #[arg(long, conflicts_with_all = ["outcome", "choose", "of", "reason", "issue"])]
        undo: bool,
    },
    /// Say which thread your changes from now on are for, until the next
    /// switch or the thread is resolved; without a thread, show the one
    /// they are for, or pick one on a terminal.
    Switch {
        /// Thread number, such as 3.4, or 4 for one of your own space: an
        /// open thread your space takes part in.
        #[arg(conflicts_with = "none")]
        thread: Option<ThreadNumber>,
        /// Your changes from now on are for no thread.
        #[arg(long)]
        none: bool,
        /// Space, a workspace or chat, whose agent's changes to switch;
        /// defaults to your own.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
    },
    /// Put changes into a thread, out of any other.
    Add {
        /// Thread number, such as 3.4, or 4 for one of your own space.
        thread: ThreadNumber,
        /// Change numbers, as shoal changes lists them, such as 12, or 3.12
        /// for change 12 of space 3.
        #[arg(required = true)]
        changes: Vec<ChangeNumber>,
        /// Space whose changes bare numbers name; defaults to your own.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
    },
    /// Take changes out of a thread.
    Remove {
        /// Thread number, such as 3.4, or 4 for one of your own space.
        thread: ThreadNumber,
        /// Change numbers, as shoal changes lists them.
        #[arg(required = true)]
        changes: Vec<ChangeNumber>,
        /// Space whose changes bare numbers name; defaults to your own.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
    },
    /// Choose a thread to read and reply to on a terminal, else list them.
    List {
        /// Space, a workspace or chat, whose threads to list: those it opened,
        /// holds, held or linked. Defaults to your own space, or every thread
        /// from the user's terminal.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
        /// Which threads to list.
        #[arg(long, default_value = "open")]
        state: ThreadState,
        /// Print the list instead of opening the picker on a terminal.
        #[arg(short = 'p', long)]
        print: bool,
    },
}

// What an outcome of `shoal thread resolve` takes besides its name; a doc
// comment here would replace that command's description.
#[derive(Debug, Args)]
pub struct OutcomeData {
    /// The number of the choice an answer settled on.
    #[arg(long, value_name = "N", conflicts_with_all = ["of", "reason", "issue"])]
    pub choose: Option<u32>,
    /// The thread a duplicate repeats.
    #[arg(long, value_name = "THREAD", conflicts_with_all = ["reason", "issue"])]
    pub of: Option<ThreadNumber>,
    /// Why it was withdrawn, why a finding's claim is wrong
    /// (false-positive) or why its code stays (wont-fix).
    #[arg(long, conflicts_with = "issue")]
    pub reason: Option<String>,
    /// The issue whose work fixes a finding (deferred).
    #[arg(short = 'i', long, value_parser = super::issue_input)]
    pub issue: Option<crate::forge::items::reference::ItemInput>,
}

// What `shoal send` takes for a post in a thread; a doc comment would
// replace the command's help.
#[derive(Debug, Default, Args)]
pub struct PostArgs {
    /// A possible answer the post offers; repeat for each. The other side
    /// picks one with --choose.
    #[arg(long = "choice", value_name = "CHOICE")]
    pub choices: Vec<String>,
    /// The answer the post recommends.
    #[arg(long, value_name = "ANSWER")]
    pub recommend: Option<String>,
    /// Why it recommends it.
    #[arg(long, requires = "recommend")]
    pub reason: Option<String>,
    /// Pick the choice of this number the other side offered last; text is
    /// never read as a choice number.
    #[arg(long, value_name = "N")]
    pub choose: Option<u32>,
    /// A file, or its lines as PATH:FIRST-LAST, in a worktree your scope
    /// reaches that the post points at; repeat for more. The other side
    /// receives each at its lines then; --attach sends a copy instead.
    #[arg(long, value_name = "PATH[:FIRST-LAST]")]
    pub code: Vec<CodeArg>,
    /// Changes, as shoal changes numbers them, that the post points at,
    /// such as 12,14, or 3.12 for change 12 of space 3; the other side
    /// receives their diff where their code is then.
    #[arg(long, value_name = "CHANGES", value_delimiter = ',')]
    pub changes: Vec<ChangeNumber>,
    /// Wait for the other side's next post and print it.
    #[arg(long)]
    pub wait: bool,
    /// Continue waiting after this post, as the installed release does after
    /// an upgrade restarted the daemon.
    #[arg(long, hide = true, value_name = "POST")]
    pub resume: Option<i64>,
}

impl PostArgs {
    /// Whether any option only a post in a thread takes is given.
    pub fn given(&self) -> bool {
        !self.choices.is_empty()
            || self.recommend.is_some()
            || self.choose.is_some()
            || !self.code.is_empty()
            || !self.changes.is_empty()
            || self.wait
            || self.resume.is_some()
    }
}

/// A place in code `--code` names: a path, and its lines after the last
/// colon when they read as lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeArg {
    pub path: PathBuf,
    pub lines: Option<Lines>,
}

impl FromStr for CodeArg {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let (path, lines) = match text.rsplit_once(':') {
            Some((path, lines)) => match lines.parse() {
                Ok(lines) => (path, Some(lines)),
                Err(_) => (text, None),
            },
            None => (text, None),
        };
        ensure!(!path.is_empty(), "name a file, as PATH or PATH:FIRST-LAST");
        Ok(Self {
            path: path.into(),
            lines,
        })
    }
}

#[cfg(test)]
mod tests;
