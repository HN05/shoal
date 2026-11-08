//! The daemon's answers to [`QuestionMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    item_numbers::ItemNumber,
    questions::Question,
    scope::{Caller, Scoped, inspect_owner},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::protocol::{Body, QuestionMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: QuestionMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        QuestionMethod::AwaitAnswer { space, item, ask } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                let question = manager.await_answer(&space, item, &ask).await?;
                Ok(Body::Question(question))
            }));
        }
        QuestionMethod::AskQuestion {
            space,
            ask,
            attachments,
            sender_chat,
        } => {
            let attacher = manager.attacher(caller, sender_chat.as_ref()).await;
            let asked = manager.ask_question(&space, ask, attachments, &attacher);
            Body::Question(asked.await?)
        }
        QuestionMethod::AskInChat { space } => {
            manager
                .mark_chat_question(&space, caller.and_then(Caller::execution_id))
                .await?;
            Body::Ok
        }
        QuestionMethod::RepliedInChat { space } => {
            manager
                .reply_in_chat(&space, caller.and_then(Caller::execution_id))
                .await?;
            Body::Ok
        }
        QuestionMethod::ListQuestions { space } => {
            Body::Questions(manager.questions(space.as_ref()).await?)
        }
        QuestionMethod::ShowQuestion { number } => {
            Body::Question(show_question(manager, caller, number).await?)
        }
        QuestionMethod::WithdrawQuestion {
            space,
            number,
            shown,
        } => Body::Question(manager.withdraw_question(&space, number, shown).await?),
        QuestionMethod::AcceptQuestionRecommendation { number, shown } => Body::Question(
            manager
                .accept_question_recommendation(number, shown)
                .await?,
        ),
        QuestionMethod::AnswerQuestion {
            number,
            answer,
            shown,
        } => Body::Question(manager.answer_question(number, answer, shown).await?),
    }))
}

impl Scoped for QuestionMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            QuestionMethod::AskQuestion { space, .. }
            | QuestionMethod::AskInChat { space }
            | QuestionMethod::RepliedInChat { space }
            | QuestionMethod::WithdrawQuestion { space, .. }
            | QuestionMethod::AwaitAnswer { space, .. } => Some(space),
            QuestionMethod::ListQuestions { space } => Some(caller.own_by_default(space)),
            QuestionMethod::ShowQuestion { number } => {
                show_question(manager, Some(caller), *number).await?;
                None
            }
            QuestionMethod::AnswerQuestion { number, shown, .. }
            | QuestionMethod::AcceptQuestionRecommendation { number, shown } => {
                *shown = Some(
                    manager
                        .answerable_question(caller, *number, shown.take())
                        .await?,
                );
                None
            }
        })
    }
}

async fn show_question(
    manager: &Manager,
    caller: Option<&Caller>,
    number: ItemNumber,
) -> Result<Question> {
    let question = manager.question(number).await?;
    inspect_owner(manager, caller, &question.space_id, "questions").await?;
    Ok(question)
}
