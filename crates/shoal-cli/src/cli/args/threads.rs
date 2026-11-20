//! Arguments of `shoal thread` and of what `shoal send` takes for a post in
//! a thread.
use clap::{Args, Subcommand};

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
        /// What the thread is for, which decides how it can end.
        #[arg(long, default_value = "question")]
        kind: ThreadKind,
        /// Space, a workspace or chat, that holds the thread.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
    },
    /// Give a thread you hold to a space below you, or up to your
    /// coordinator or the user; the new holder receives its posts from then
    /// on.
    Forward {
        /// Thread number, such as 3.2, or 2 for one of your own space.
        thread: ThreadNumber,
        /// Space, a workspace or chat, to give it to; omit to pass it up.
        #[arg(short = 's', long)]
        space: Option<SpaceSelector>,
        /// A note for the new holder, such as why it should answer.
        #[arg(long)]
        note: Option<String>,
    },
    /// Settle a thread with an outcome, or reopen it with --undo; its
    /// opener resolves it.
    Resolve {
        /// Thread number, such as 3.2, or 2 for one of your own space.
        thread: ThreadNumber,
        /// answered (a question), done (a request), duplicate or withdrawn.
        #[arg(required_unless_present = "undo")]
        outcome: Option<OutcomeKind>,
        /// The number of the choice an answer settled on.
        #[arg(long, value_name = "N", conflicts_with_all = ["of", "reason"])]
        choose: Option<u32>,
        /// The thread a duplicate repeats.
        #[arg(long, value_name = "THREAD", conflicts_with = "reason")]
        of: Option<ThreadNumber>,
        /// Why it was withdrawn.
        #[arg(long)]
        reason: Option<String>,
        /// Reopen a resolved thread.
        #[arg(long, conflicts_with_all = ["outcome", "choose", "of", "reason"])]
        undo: bool,
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
    /// Wait for the other side's next post and print it.
    #[arg(long)]
    pub wait: bool,
    /// Post your turn's final message in the thread instead, which a reply
    /// in your conversation answers.
    #[arg(long, conflicts_with_all = ["body_file", "attach", "choices", "recommend", "choose", "wait"])]
    pub in_chat: bool,
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
            || self.wait
            || self.in_chat
            || self.resume.is_some()
    }
}
