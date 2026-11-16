//! Questions agents ask the user.
use anyhow::Result;
use shoal_protocol::questions::{Ask, Listing, Question};

use shoal_ids::space_ids::SpaceId;

use crate::connection::{Daemon, await_reply, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{files::NewFile, item_numbers::ItemNumber, protocol::QuestionMethod};

/// The question `number`.
pub async fn show(daemon: &impl Daemon, number: ItemNumber) -> Result<Question> {
    request(daemon, QuestionMethod::ShowQuestion { number }).await
}

/// The open questions of `space`, or of every space.
pub async fn list(daemon: &impl Daemon, space: Option<SpaceSelector>) -> Result<Listing> {
    request(daemon, QuestionMethod::ListQuestions { space }).await
}

/// Ask the user `ask` for the agents of `space` with `attachments`; the
/// chat `sender_chat` names, when its agent asks, attaches its files.
pub async fn ask(
    daemon: &impl Daemon,
    space: SpaceSelector,
    ask: Ask,
    attachments: Vec<NewFile>,
    sender_chat: Option<SpaceId>,
) -> Result<Question> {
    let method = QuestionMethod::AskQuestion {
        space,
        ask,
        attachments,
        sender_chat,
    };
    request(daemon, method).await
}

/// The question `item` of `space`, which asks `ask`, once it is answered,
/// waiting across daemon restarts; `restarted` says the daemon restarted
/// before the wait began.
pub async fn await_answer(
    daemon: &impl Daemon,
    space: &SpaceSelector,
    item: u32,
    ask: &Ask,
    restarted: bool,
) -> Result<Question> {
    let method = || QuestionMethod::AwaitAnswer {
        space: space.clone(),
        item,
        ask: ask.clone(),
    };
    Question::try_from(await_reply(daemon, method, restarted).await?)
}

/// Mark the turn of `space`'s calling agent as asking the user in its
/// conversation.
pub async fn ask_in_chat(daemon: &impl Daemon, space: SpaceSelector) -> Result<()> {
    request(daemon, QuestionMethod::AskInChat { space }).await
}

/// Close the questions `space`'s agents asked in their conversation, where
/// the user replied.
pub async fn replied_in_chat(daemon: &impl Daemon, space: SpaceSelector) -> Result<()> {
    request(daemon, QuestionMethod::RepliedInChat { space }).await
}

/// Withdraw `shown`, which its space's agents no longer need answered.
pub async fn withdraw(daemon: &impl Daemon, shown: Question) -> Result<Question> {
    let method = QuestionMethod::WithdrawQuestion {
        space: SpaceSelector::id(&shown.space_id),
        number: shown.number,
        shown: Some(shown),
    };
    request(daemon, method).await
}

/// Answer the question `number` with `answer`; `shown` is the question the
/// user was shown, which a later question reusing its number does not match.
pub async fn answer(
    daemon: &impl Daemon,
    number: ItemNumber,
    answer: String,
    shown: Option<Question>,
) -> Result<Question> {
    let method = QuestionMethod::AnswerQuestion {
        number,
        answer,
        shown,
    };
    request(daemon, method).await
}

/// Answer `shown` with its recommendation.
pub async fn accept_recommendation(daemon: &impl Daemon, shown: Question) -> Result<Question> {
    let method = QuestionMethod::AcceptQuestionRecommendation {
        number: shown.number,
        shown: Some(shown),
    };
    request(daemon, method).await
}
