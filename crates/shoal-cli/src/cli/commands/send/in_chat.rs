//! What `shoal send --in-chat` tells the agent it marked.
use anyhow::Result;
use shoal_protocol::chat_marks::{ChatMark, ChatMarked};

use crate::cli::context::Context;

pub(in crate::cli) fn show(ctx: &Context, marked: &ChatMarked) -> Result<i32> {
    let mut message = match &marked.mark {
        ChatMark::Post { thread } => format!(
            "Write your post as your final message and end your turn; it goes to thread {thread}."
        ),
        ChatMark::Alert { to } => format!(
            "Write your alert as your final message and end your turn; it goes to {} once the turn ends.",
            to.describe()
        ),
    };
    match &marked.replaced {
        Some(ChatMark::Post { thread }) => {
            message.push_str(&format!(" This replaces your post for thread {thread}."))
        }
        Some(ChatMark::Alert { to }) => {
            message.push_str(&format!(" This replaces your alert to {}.", to.describe()))
        }
        None => {}
    }
    ctx.emit(&message, marked)?;
    Ok(0)
}
