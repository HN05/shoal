//! The user's answers to questions and decisions on approval requests,
//! bound to the record the user was shown when the call names it.
use shoal_client::{
    AnswerQuestion, ApiError, DecideApproval, ErrorCode,
    records::{Approval, Question},
};

use super::super::{Session, errors, records};
use crate::{
    daemon::{approvals::ApprovalRequest, questions},
    protocol::{ApprovalMethod, Body, QuestionMethod},
};

pub(super) async fn answer_question(
    session: &Session,
    params: AnswerQuestion,
) -> Result<Question, ApiError> {
    let shown = match &params.shown {
        Some(shown) => Some(shown_question(session, params.id, shown).await?),
        None => None,
    };
    let question: questions::Question = session
        .ask(QuestionMethod::AnswerQuestion {
            id: params.id,
            answer: params.answer,
            shown,
        })
        .await?;
    Ok(records::question(question))
}

/// Question `id`, which must still be the one `shown` showed.
async fn shown_question(
    session: &Session,
    id: i64,
    shown: &Question,
) -> Result<questions::Question, ApiError> {
    let question = session
        .server
        .manager
        .question(id)
        .await
        .map_err(|error| errors::failed(&error))?;
    if records::question(question.clone()) != *shown {
        return Err(changed("question", id));
    }
    Ok(question)
}

pub(super) async fn decide_approval(
    session: &Session,
    params: DecideApproval,
) -> Result<Approval, ApiError> {
    let shown = match &params.shown {
        Some(shown) => Some(Box::new(shown_approval(session, params.id, shown).await?)),
        None => None,
    };
    let body = session
        .run(ApprovalMethod::DecideApproval {
            id: params.id,
            approve: params.approve,
            shown,
            details: Default::default(),
        })
        .await?;
    let request: Box<ApprovalRequest> = match body {
        Body::ApprovalRequest(request) => request,
        body => return Err(errors::unexpected(body.unexpected("ApprovalRequest"))),
    };
    Ok(records::approval(*request))
}

/// Approval request `id`, which must still be the one `shown` showed.
async fn shown_approval(
    session: &Session,
    id: i64,
    shown: &Approval,
) -> Result<ApprovalRequest, ApiError> {
    let request = session
        .server
        .manager
        .show_approval(id)
        .await
        .map_err(|error| errors::failed(&error))?;
    if records::approval(request.clone()) != *shown {
        return Err(changed("approval request", id));
    }
    Ok(request)
}

/// The refusal of a decision on a record that changed since it was shown.
fn changed(kind: &str, id: i64) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!("{kind} {id} is no longer the one shown; it was decided, withdrawn or replaced"),
    )
}
