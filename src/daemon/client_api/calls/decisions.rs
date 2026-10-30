//! The user's answers to questions and decisions on approval requests.
use shoal_client::{
    AnswerQuestion, ApiError, DecideApproval, ErrorCode,
    records::{Approval, Question},
};

use super::super::{Session, records};
use crate::{
    daemon::{approvals::ApprovalRequest, questions},
    protocol::{ApprovalMethod, Body, QuestionMethod},
};

pub(super) async fn answer_question(
    session: &Session,
    params: AnswerQuestion,
) -> Result<Question, ApiError> {
    let question: questions::Question = session
        .ask(QuestionMethod::AnswerQuestion {
            id: params.id,
            answer: params.answer,
            shown: None,
        })
        .await?;
    Ok(records::question(question))
}

/// Only the user decides approval requests through the client API.
pub(super) async fn decide_approval(
    session: &Session,
    params: DecideApproval,
) -> Result<Approval, ApiError> {
    if session.caller.is_some() {
        return Err(ApiError::new(
            ErrorCode::ScopeDenied,
            "only the user decides approval requests",
        ));
    }
    let body = session
        .run(ApprovalMethod::DecideApproval {
            id: params.id,
            approve: params.approve,
            shown: None,
            details: Default::default(),
        })
        .await?;
    let request: Box<ApprovalRequest> = match body {
        Body::ApprovalRequest(request) => request,
        body => {
            return Err(super::super::errors::unexpected(
                body.unexpected("ApprovalRequest"),
            ));
        }
    };
    Ok(records::approval(*request))
}
