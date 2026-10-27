//! The daemon's answers to [`QuestionMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    questions::Question,
    scope::{Caller, Scoped, inspect_owner},
    workspace::Manager,
};
use crate::protocol::{Body, QuestionMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: QuestionMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        QuestionMethod::AwaitAnswer { workspace, id, ask } => {
            let manager = manager.clone();
            return Ok(Answer::wait(async move {
                let question = manager.await_answer(&workspace, id, &ask).await?;
                Ok(Body::Question(question))
            }));
        }
        QuestionMethod::AskQuestion { workspace, ask } => {
            Body::Question(manager.ask_question(&workspace, ask).await?)
        }
        QuestionMethod::ListQuestions { workspace } => {
            Body::Questions(manager.questions(workspace.as_deref()).await?)
        }
        QuestionMethod::ShowQuestion { id } => {
            Body::Question(show_question(manager, caller, id).await?)
        }
        QuestionMethod::WithdrawQuestion {
            workspace,
            id,
            shown,
        } => Body::Question(manager.withdraw_question(&workspace, id, shown).await?),
        QuestionMethod::AcceptQuestionRecommendation { id, shown } => {
            Body::Question(manager.accept_question_recommendation(id, shown).await?)
        }
        QuestionMethod::AnswerQuestion { id, answer, shown } => {
            Body::Question(manager.answer_question(id, answer, shown).await?)
        }
    }))
}

impl Scoped for QuestionMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            QuestionMethod::AskQuestion { workspace, .. }
            | QuestionMethod::WithdrawQuestion { workspace, .. }
            | QuestionMethod::AwaitAnswer { workspace, .. } => Some(workspace),
            QuestionMethod::ListQuestions { workspace } => {
                Some(workspace.get_or_insert_with(|| caller.workspace_id.clone()))
            }
            QuestionMethod::ShowQuestion { id } => {
                show_question(manager, Some(caller), *id).await?;
                None
            }
            QuestionMethod::AnswerQuestion { id, shown, .. }
            | QuestionMethod::AcceptQuestionRecommendation { id, shown } => {
                *shown = Some(
                    manager
                        .answerable_question(caller, *id, shown.take())
                        .await?,
                );
                None
            }
        })
    }
}

async fn show_question(manager: &Manager, caller: Option<&Caller>, id: i64) -> Result<Question> {
    let question = manager.question(id).await?;
    inspect_owner(manager, caller, &question.workspace_id, "questions").await?;
    Ok(question)
}
