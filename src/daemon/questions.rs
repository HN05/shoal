//! Questions a workspace's agents ask the user. A question names the decision
//! and the agent's recommendation; only the user answers, and the answer
//! reaches the agents as a message. Delivering the answer, or removing the
//! workspace, deletes the question, which frees its number for the next one.
//! Marking the assignment done, or the asking agent withdrawing a question,
//! closes it unanswered; a closed question keeps its number until the
//! workspace goes so that a late answer is refused.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::{
    daemon::{
        agent_messages,
        notifications::{self, About, NotificationKind},
        workspace::Manager,
    },
    model::Workspace,
};

/// Longest answer, in bytes; answers may span lines.
pub const MAX_ANSWER_LEN: usize = 4096;
/// Most choices a question may offer.
pub const MAX_CHOICES: usize = 20;
/// Bytes of questions one listing returns, leaving room for the response
/// around them.
const LISTING_BUDGET: usize = crate::protocol::MAX_FRAME - 1024;
/// Rechecks a waited-for question when no answer wakes the wait, so removal
/// of its workspace ends it.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// What an agent asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub question: String,
    pub recommendation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// The number the user answers it by; reused once the question is deleted.
    pub id: i64,
    pub workspace_id: String,
    /// The workspace's current name.
    pub workspace: String,
    /// Unix seconds.
    pub created_at: i64,
    #[serde(flatten)]
    pub ask: Ask,
    /// `None` while the question is open.
    pub answer: Option<String>,
    pub answered_at: Option<i64>,
    /// The agent message carrying the answer; delivering it deletes the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    /// Unix seconds `done` or withdrawal closed the unanswered question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    /// Whether the asking agent closed it, needing no answer after all.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub withdrawn: bool,
}

/// Questions that fit one response, and how many older ones were left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub questions: Vec<Question>,
    pub omitted: usize,
}

impl Ask {
    pub fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("question", Some(&self.question)),
            ("recommendation", Some(&self.recommendation)),
            ("reason", self.reason.as_ref()),
        ] {
            if let Some(value) = value {
                crate::validate::message(value).with_context(|| format!("invalid {field}"))?;
            }
        }
        ensure!(
            self.choices.len() <= MAX_CHOICES,
            "a question offers at most {MAX_CHOICES} choices"
        );
        for choice in &self.choices {
            crate::validate::message(choice).context("invalid choice")?;
        }
        Ok(())
    }

    /// The answer an agent receives: a choice number names that choice.
    fn resolve(&self, answer: &str) -> Result<String> {
        if let Ok(number) = answer.trim().parse::<usize>()
            && !self.choices.is_empty()
        {
            return number
                .checked_sub(1)
                .and_then(|index| self.choices.get(index))
                .cloned()
                .with_context(|| format!("choose a number from 1 to {}", self.choices.len()));
        }
        validate_answer(answer)?;
        Ok(answer.to_owned())
    }
}

/// Answers may span lines but carry no other control characters, which would
/// corrupt terminal output.
fn validate_answer(answer: &str) -> Result<()> {
    ensure!(
        !answer.trim().is_empty()
            && answer.len() <= MAX_ANSWER_LEN
            && !answer.chars().any(|c| c.is_control() && c != '\n'),
        "answers must be nonempty text without control characters other than newlines (max {MAX_ANSWER_LEN} bytes)"
    );
    Ok(())
}

impl Question {
    pub fn is_open(&self) -> bool {
        self.answer.is_none() && self.closed_at.is_none()
    }

    /// Refuse an answer to a question that is answered or closed.
    pub fn ensure_open(&self) -> Result<()> {
        ensure!(
            self.answer.is_none(),
            "question {} is already answered",
            self.id
        );
        ensure!(
            !self.withdrawn,
            "question {} was withdrawn: its agent no longer needs an answer",
            self.id
        );
        ensure!(
            self.closed_at.is_none(),
            "question {} is closed: its workspace's assignment was marked done",
            self.id
        );
        Ok(())
    }

    /// Whether `other` is this question as asked, though it may since be
    /// answered: the same workspace, ask and time.
    fn is_same(&self, other: &Question) -> bool {
        self.workspace_id == other.workspace_id
            && self.ask == other.ask
            && self.created_at == other.created_at
    }

    /// What the user is told about a new question, including how to answer it.
    /// It is cut to the length of other messages, so a batch of notifications
    /// still fits one response.
    fn notification(&self) -> String {
        shorten(
            format!(
                "question {} (answer with shoal questions {}): {} Recommended: {}",
                self.id, self.id, self.ask.question, self.ask.recommendation
            ),
            crate::validate::MAX_MESSAGE_LEN,
        )
    }

    /// What the user, or the coordinating agent, is told about a withdrawn
    /// question whose notification was already shown.
    fn withdrawal_notification(&self) -> String {
        shorten(
            format!(
                "withdrew question {} (no answer needed): {}",
                self.id, self.ask.question
            ),
            crate::validate::MAX_MESSAGE_LEN,
        )
    }

    /// The agent message carrying `answer`; it names the question it answers.
    fn answer_message(&self, answer: &str) -> String {
        format!(
            "Answer to question {} ({}): {answer}",
            self.id, self.ask.question
        )
    }
}

/// `text` cut to at most `max` bytes, ending with an ellipsis when cut.
fn shorten(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push('…');
    text
}

const SELECT: &str =
    "SELECT q.number,q.workspace_id,w.name,q.created_at,q.question,q.recommendation,
    q.reason,q.choices,q.answer,q.answered_at,
    (SELECT m.id FROM agent_messages m WHERE m.id=q.message_id),q.closed_at,q.withdrawn
    FROM questions q JOIN workspaces w ON w.id=q.workspace_id";

fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Question, String)> {
    Ok((
        Question {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            workspace: row.get(2)?,
            created_at: row.get(3)?,
            ask: Ask {
                question: row.get(4)?,
                recommendation: row.get(5)?,
                reason: row.get(6)?,
                choices: Vec::new(),
            },
            answer: row.get(8)?,
            answered_at: row.get(9)?,
            message_id: row.get(10)?,
            closed_at: row.get(11)?,
            withdrawn: row.get(12)?,
        },
        row.get(7)?,
    ))
}

fn query(db: &Connection, filter: &str, params: impl rusqlite::Params) -> Result<Vec<Question>> {
    db.prepare(&format!("{SELECT} {filter} ORDER BY q.id"))?
        .query_map(params, decode)?
        .map(|row| {
            let (mut question, choices) = row?;
            question.ask.choices = serde_json::from_str(&choices)
                .with_context(|| format!("question {} has invalid choices", question.id))?;
            Ok(question)
        })
        .collect()
}

fn by_id(db: &Connection, id: i64) -> Result<Option<Question>> {
    Ok(query(db, "WHERE q.number=?1", [id])?.pop())
}

/// An identical question the agents have not received the answer to, so
/// asking again, as after an interrupted wait, returns it.
fn repeated(tx: &Transaction<'_>, workspace_id: &str, ask: &Ask) -> Result<Option<Question>> {
    let candidates = query(
        tx,
        "WHERE q.workspace_id=?1 AND q.question=?2",
        params![workspace_id, ask.question],
    )?;
    let Some(question) = candidates
        .into_iter()
        .rev()
        .find(|question| question.is_open() || question.message_id.is_some())
    else {
        return Ok(None);
    };
    ensure!(
        question.ask == *ask,
        "question {} asks the same with different details; ask a different question",
        question.id
    );
    Ok(Some(question))
}

/// Record `ask` under the smallest free number and return that number.
fn insert(tx: &Transaction<'_>, workspace_id: &str, ask: &Ask) -> Result<i64> {
    let number = crate::daemon::store::free_number(tx, "questions")?;
    tx.execute(
        "INSERT INTO questions(number,workspace_id,created_at,question,recommendation,reason,choices)
            VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            number,
            workspace_id,
            i64::try_from(crate::time::unix_seconds())?,
            ask.question,
            ask.recommendation,
            ask.reason,
            serde_json::to_string(&ask.choices)?
        ],
    )?;
    Ok(number)
}

fn open_question(tx: &Transaction<'_>, id: i64) -> Result<Question> {
    let question = by_id(tx, id)?.with_context(|| format!("unknown question {id}"))?;
    question.ensure_open()?;
    Ok(question)
}

/// Persist `answer` and the message carrying it on `question`.
fn store_answer(
    tx: &Transaction<'_>,
    question: &mut Question,
    answer: String,
    message_id: Option<i64>,
) -> Result<()> {
    let answered_at = i64::try_from(crate::time::unix_seconds())?;
    tx.execute(
        "UPDATE questions SET answer=?2,answered_at=?3,message_id=?4 WHERE number=?1",
        params![question.id, answer, answered_at, message_id],
    )?;
    question.answer = Some(answer);
    question.answered_at = Some(answered_at);
    question.message_id = message_id;
    Ok(())
}

enum Answer {
    Text(String),
    Recommendation,
}

impl Answer {
    fn resolve(&self, ask: &Ask) -> Result<String> {
        match self {
            Self::Text(text) => ask.resolve(text),
            Self::Recommendation => {
                validate_answer(&ask.recommendation)?;
                Ok(ask.recommendation.clone())
            }
        }
    }
}

/// Record `answer` and queue the message that carries it, together. With
/// `shown`, the question the user was shown, a later question reusing its
/// number is refused.
fn record_answer(
    tx: &Transaction<'_>,
    id: i64,
    answer: &Answer,
    shown: Option<&Question>,
) -> Result<Question> {
    let mut question = open_question(tx, id)?;
    ensure!(
        shown.is_none_or(|shown| question.is_same(shown)),
        "question {id} changed since it was shown; choose again"
    );
    let answer = answer.resolve(&question.ask)?;
    let message = question.answer_message(&answer);
    let message_id = agent_messages::insert(tx, &question.workspace_id, &message)?;
    store_answer(tx, &mut question, answer, message_id)?;
    Ok(question)
}

/// Close the workspace's open questions, which its finished assignment no
/// longer waits on, and return their numbers.
pub(crate) fn close_open(tx: &Transaction<'_>, workspace_id: &str) -> Result<Vec<i64>> {
    let closed_at = i64::try_from(crate::time::unix_seconds())?;
    let mut numbers = tx
        .prepare(
            "UPDATE questions SET closed_at=?2
                WHERE workspace_id=?1 AND answer IS NULL AND closed_at IS NULL
                RETURNING number",
        )?
        .query_map(params![workspace_id, closed_at], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    numbers.sort_unstable();
    Ok(numbers)
}

/// Close the workspace's open question `id` as withdrawn by its agent.
fn withdraw(tx: &Transaction<'_>, workspace: &Workspace, id: i64) -> Result<Question> {
    let mut question = by_id(tx, id)?
        .filter(|question| question.workspace_id == workspace.id)
        .with_context(|| format!("workspace {} has no question {id}", workspace.name))?;
    question.ensure_open()?;
    let closed_at = i64::try_from(crate::time::unix_seconds())?;
    tx.execute(
        "UPDATE questions SET closed_at=?2,withdrawn=1 WHERE number=?1",
        params![id, closed_at],
    )?;
    question.closed_at = Some(closed_at);
    question.withdrawn = true;
    Ok(question)
}

/// Delete the question whose answer `message_id` carried, once the message
/// is delivered.
pub(crate) fn delete_delivered(
    tx: &Transaction<'_>,
    workspace_id: &str,
    message_id: i64,
) -> Result<()> {
    tx.execute(
        "DELETE FROM questions WHERE workspace_id=?1 AND message_id=?2",
        params![workspace_id, message_id],
    )?;
    Ok(())
}

/// The newest of `questions` that fit one protocol response, oldest first.
fn newest_that_fit(mut questions: Vec<Question>) -> Result<Listing> {
    let mut used = 0;
    let mut start = questions.len();
    for (index, question) in questions.iter().enumerate().rev() {
        used += serde_json::to_vec(question)?.len() + 1;
        if used > LISTING_BUDGET {
            break;
        }
        start = index;
    }
    let questions = questions.split_off(start);
    Ok(Listing {
        questions,
        omitted: start,
    })
}

impl Manager {
    /// Record the question, or return the same one asked again, and tell the
    /// user about a new one.
    pub async fn ask_question(&self, selector: &str, ask: Ask) -> Result<Question> {
        ask.validate()?;
        let workspace = self.workspace(selector).await?;
        let (question, new) = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                // A workspace reserved for removal takes no question its
                // cleanup checks have not seen.
                crate::daemon::store::require_ready(&tx, &workspace.id)?;
                if let Some(question) = repeated(&tx, &workspace.id, &ask)? {
                    return Ok((question, false));
                }
                let id = insert(&tx, &workspace.id, &ask)?;
                let question = by_id(&tx, id)?.context("recorded question is missing")?;
                tx.commit()?;
                Ok((question, true))
            })
            .await?;
        if new {
            self.announce_question(&question).await;
        }
        Ok(question)
    }

    /// Tell the user about `question` unless it was withdrawn or answered
    /// since it was asked.
    async fn announce_question(&self, question: &Question) {
        let _gate = self.announcement_gate.lock().await;
        let id = question.id;
        let stored = self.store.run(move |db| by_id(db, id)).await;
        match stored {
            Ok(Some(stored)) if stored.is_same(question) && stored.is_open() => {}
            Ok(_) => return,
            Err(error) => crate::daemon::log!("question {id} not rechecked: {error:#}"),
        }
        let about = About::workspace(&question.workspace, &question.workspace_id);
        self.notify_about(
            about,
            NotificationKind::QuestionAsked,
            question.notification(),
        )
        .await;
    }

    /// The newest open questions that fit one response, oldest first.
    pub async fn questions(&self, selector: Option<&str>) -> Result<Listing> {
        let owner = self.workspace_filter(selector).await?;
        let questions = self
            .store
            .run(move |db| {
                query(
                    db,
                    "WHERE (?1 IS NULL OR q.workspace_id=?1) AND q.answer IS NULL
                        AND q.closed_at IS NULL",
                    [owner],
                )
            })
            .await?;
        newest_that_fit(questions)
    }

    pub async fn question(&self, id: i64) -> Result<Question> {
        self.store
            .run(move |db| by_id(db, id))
            .await?
            .with_context(|| format!("unknown question {id}"))
    }

    pub async fn has_open_questions(&self, workspace_id: &str) -> Result<bool> {
        let id = workspace_id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    "SELECT 1 FROM questions
                        WHERE workspace_id=?1 AND answer IS NULL AND closed_at IS NULL",
                    [id],
                )
            })
            .await
    }

    /// Record the user's answer to question `id`, or to `shown` when the user
    /// chose it from a listing, and queue it for the workspace's agents.
    pub async fn answer_question(
        &self,
        id: i64,
        answer: String,
        shown: Option<Question>,
    ) -> Result<Question> {
        self.record_question_answer(id, Answer::Text(answer), shown)
            .await
    }

    pub async fn accept_question_recommendation(
        &self,
        id: i64,
        shown: Option<Question>,
    ) -> Result<Question> {
        self.record_question_answer(id, Answer::Recommendation, shown)
            .await
    }

    async fn record_question_answer(
        &self,
        id: i64,
        answer: Answer,
        shown: Option<Question>,
    ) -> Result<Question> {
        let question = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let question = record_answer(&tx, id, &answer, shown.as_ref())?;
                tx.commit()?;
                Ok(question)
            })
            .await?;
        self.agent_messages_changed.send_modify(|count| *count += 1);
        self.questions_changed.send_modify(|count| *count += 1);
        Ok(question)
    }

    /// Withdraw the workspace's open question `id`, which its agents no
    /// longer need answered. Its notification goes while unread; once shown,
    /// the user or the coordinating agent is told instead.
    pub async fn withdraw_question(&self, selector: &str, id: i64) -> Result<Question> {
        let workspace = self.workspace(selector).await?;
        let _gate = self.announcement_gate.lock().await;
        let (question, retracted) = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let question = withdraw(&tx, &workspace, id)?;
                let retracted = notifications::retract(
                    &tx,
                    &question.workspace_id,
                    NotificationKind::QuestionAsked,
                    &question.notification(),
                )?;
                tx.commit()?;
                Ok((question, retracted))
            })
            .await?;
        self.questions_changed.send_modify(|count| *count += 1);
        if !retracted {
            self.notify(
                Some(&question.workspace),
                NotificationKind::QuestionWithdrawn,
                question.withdrawal_notification(),
            )
            .await;
        }
        Ok(question)
    }

    /// Return the workspace's question `id` asking `ask` once it is answered.
    /// What it asks tells it from a later question reusing the number; a
    /// later identical question asks the same, so its answer serves the wait
    /// too.
    pub async fn await_answer(&self, selector: &str, id: i64, ask: &Ask) -> Result<Question> {
        let workspace = self.workspace(selector).await?;
        // Subscribe before reading, so an answer in between still wakes.
        let mut changed = self.questions_changed.subscribe();
        loop {
            let question = self
                .store
                .run(move |db| by_id(db, id))
                .await?
                .filter(|question| {
                    question.workspace_id == workspace.id && question.ask == *ask
                })
                .with_context(|| {
                    format!(
                        "workspace {} has no question {id} awaiting delivery; a delivered answer removes its question",
                        workspace.name
                    )
                })?;
            if question.answer.is_some() {
                return Ok(question);
            }
            // A closed question is never answered.
            question.ensure_open()?;
            // A closed channel only disables its wake; the poll still rechecks.
            tokio::select! {
                Ok(()) = changed.changed() => {}
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests;
