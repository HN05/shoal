//! Questions the agents of a space, a workspace or chat, ask the user. A
//! question names the decision and the agent's recommendation; only the user
//! answers, and the answer reaches the agents as a message. Delivering the
//! answer, or removing the workspace or ending the chat, deletes the
//! question, which frees its number for the next one. Marking the assignment
//! done, or the asking agent withdrawing a question, closes it unanswered; a
//! closed question keeps its number until the space goes so that a late
//! answer is refused. A question an agent asked in its conversation closes
//! the same way when the user replies there.
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::refusal::{Reason, Refusal};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, Transaction, named_params, params};
use std::time::Duration;

pub(crate) use crate::client::questions::{Ask, Listing, Question, QuestionStatus};
use crate::{
    daemon::{
        agent_messages, chat_messages,
        item_numbers::{self, ItemKind, ItemNumber},
        notifications::{self, About, NotificationKind},
        workspace::Manager,
        workspace_files::{self, Attacher, FileSource, NewFile, SentFile, sent_files},
    },
    model::PresentSpace,
};
use status::{OPEN, StatusColumns};

mod status;

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

/// Operations on the client type [`Ask`] that only the `shoal` command uses.
trait AskExt {
    fn validate(&self) -> Result<()>;

    /// The answer an agent receives: a choice number names that choice.
    fn resolve(&self, answer: &str) -> Result<String>;
}

impl AskExt for Ask {
    fn validate(&self) -> Result<()> {
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

/// Operations on the client type [`Question`] that only the `shoal` command uses.
pub(crate) trait QuestionExt {
    /// The question on one line, as listings and notifications show it: a
    /// question asked in the chat may span lines.
    fn one_line(&self) -> String;

    /// The answer the agent recommends; a question asked in the chat has
    /// none.
    fn recommendation(&self) -> Option<&str>;

    fn is_open(&self) -> bool;

    /// Refuse an answer to a question that is answered or closed.
    fn ensure_open(&self) -> Result<()>;

    /// Whether `other` is this question as asked, though it may since be
    /// answered: the same space, ask and time.
    fn is_same(&self, other: &Question) -> bool;

    /// What the user is told about a new question, including how to answer it.
    /// It is cut to the length of other messages, so a batch of notifications
    /// still fits one response.
    fn notification(&self) -> String;

    /// What the user, or the coordinating agent, is told about a withdrawn
    /// question whose notification was already shown.
    fn withdrawal_notification(&self) -> String;

    /// The agent message carrying `answer`; it names the question it answers.
    fn answer_message(&self, answer: &str) -> String;
}

impl QuestionExt for Question {
    fn one_line(&self) -> String {
        self.ask.question.replace(['\n', '\t'], " ")
    }

    fn recommendation(&self) -> Option<&str> {
        (!self.in_chat).then_some(self.ask.recommendation.as_str())
    }

    fn is_open(&self) -> bool {
        self.status == QuestionStatus::Open
    }

    fn ensure_open(&self) -> Result<()> {
        let number = self.number;
        match self.status {
            QuestionStatus::Open => Ok(()),
            QuestionStatus::Answered { .. } => bail!("question {number} is already answered"),
            QuestionStatus::Withdrawn { .. } => {
                bail!("question {number} was withdrawn: its agent no longer needs an answer")
            }
            QuestionStatus::Closed { .. } => {
                bail!("question {number} is closed: its workspace's assignment was marked done")
            }
        }
    }

    fn is_same(&self, other: &Question) -> bool {
        self.space_id == other.space_id
            && self.ask == other.ask
            && self.created_at == other.created_at
    }

    fn notification(&self) -> String {
        let mut text = format!(
            "question {} (answer with shoal questions {}): {}",
            self.number,
            self.number,
            self.one_line()
        );
        if let Some(recommendation) = self.recommendation() {
            text.push_str(&format!(" Recommended: {recommendation}"));
        }
        crate::validate::shorten(text, crate::validate::MAX_MESSAGE_LEN)
    }

    fn withdrawal_notification(&self) -> String {
        crate::validate::shorten(
            format!(
                "withdrew question {} (no answer needed): {}",
                self.number,
                self.one_line()
            ),
            crate::validate::MAX_MESSAGE_LEN,
        )
    }

    fn answer_message(&self, answer: &str) -> String {
        if self.in_chat {
            return format!(
                "Answer to question {}, which you asked in the chat: {answer}",
                self.number
            );
        }
        format!(
            "Answer to question {} ({}): {answer}",
            self.number, self.ask.question
        )
    }
}

/// The [`Ask`] of a question an agent asks in its chat.
trait AskFromChat {
    /// What an agent asks in its chat: its turn's final `message`, kept to
    /// the length and characters of agent messages. Without one, as from an
    /// agent whose hook does not pass it, the question points to the chat.
    fn in_chat(message: Option<&str>) -> Self;
}

impl AskFromChat for Ask {
    fn in_chat(message: Option<&str>) -> Self {
        let text: String = message
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
            .collect();
        let text = text.trim();
        let question = if text.is_empty() {
            "The agent asked a question in its chat; attach to its session to read it.".to_owned()
        } else {
            crate::validate::shorten(text.to_owned(), crate::validate::MAX_AGENT_MESSAGE_LEN)
        };
        Self {
            question,
            recommendation: String::new(),
            reason: None,
            choices: Vec::new(),
        }
    }
}

/// A question's space, workspace or chat, by ID, as a filter names it.
const SPACE: &str = "COALESCE(q.workspace_id,q.chat_id)";

const SELECT: &str = concat!(
    "SELECT q.space_number,q.number,COALESCE(q.workspace_id,q.chat_id),COALESCE(w.name,c.name),q.created_at,
    q.question,q.recommendation,q.reason,q.choices,q.answer,q.answered_at,
    CASE WHEN q.chat_id IS NULL THEN (SELECT m.id FROM agent_messages m WHERE m.id=q.message_id)
        ELSE (SELECT m.id FROM chat_messages m WHERE m.id=q.message_id) END,
    q.closed_at,q.withdrawn,q.in_chat,",
    sent_files!("q.files"),
    " FROM questions q LEFT JOIN workspaces w ON w.id=q.workspace_id
        LEFT JOIN chats c ON c.id=q.chat_id"
);

fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Question, String)> {
    Ok((
        Question {
            number: ItemNumber {
                space: row.get(0)?,
                item: row.get(1)?,
            },
            space_id: row.get(2)?,
            space: row.get(3)?,
            created_at: row.get(4)?,
            ask: Ask {
                question: row.get(5)?,
                recommendation: row.get(6)?,
                reason: row.get(7)?,
                choices: Vec::new(),
            },
            status: StatusColumns::read(row, 9)?,
            in_chat: row.get(14)?,
            files: crate::daemon::store::json_column(row, "files")?.unwrap_or_default(),
        },
        row.get(8)?,
    ))
}

fn query(db: &Connection, filter: &str, params: impl rusqlite::Params) -> Result<Vec<Question>> {
    db.prepare(&format!("{SELECT} {filter} ORDER BY q.id"))?
        .query_map(params, decode)?
        .map(|row| {
            let (mut question, choices) = row?;
            question.ask.choices = serde_json::from_str(&choices)
                .with_context(|| format!("question {} has invalid choices", question.number))?;
            Ok(question)
        })
        .collect()
}

fn by_number(db: &Connection, number: ItemNumber) -> Result<Option<Question>> {
    Ok(query(
        db,
        "WHERE q.space_number=?1 AND q.number=?2",
        params![number.space, number.item],
    )?
    .pop())
}

/// The question `number`, refusing a number that names none, or that
/// names an approval request.
fn known(db: &Connection, number: ItemNumber) -> Result<Question> {
    match by_number(db, number)? {
        Some(question) => Ok(question),
        None => Err(item_numbers::unknown(db, ItemKind::Question, number)?),
    }
}

/// An identical question with the same `files` that the agents have not
/// received the answer to, so asking again, as after an interrupted wait,
/// returns it.
fn repeated(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    ask: &Ask,
    files: &[SentFile],
) -> Result<Option<Question>> {
    let candidates = query(
        tx,
        &format!("WHERE {SPACE}=?1 AND q.question=?2"),
        params![space_id, ask.question],
    )?;
    let Some(question) = candidates.into_iter().rev().find(|question| {
        matches!(
            question.status,
            QuestionStatus::Open
                | QuestionStatus::Answered {
                    message_id: Some(_),
                    ..
                }
        )
    }) else {
        return Ok(None);
    };
    let ids = |files: &[SentFile]| files.iter().map(|file| file.id).collect::<Vec<_>>();
    ensure!(
        question.ask == *ask && ids(&question.files) == ids(files),
        "question {} asks the same with different details; ask a different question",
        question.number
    );
    Ok(Some(question))
}

/// Record `ask` from the agents of `space` with the files `files`, asked in
/// the conversation when `in_chat`, under the space's next number and
/// return that number.
fn insert(
    tx: &Transaction<'_>,
    space: &PresentSpace,
    ask: &Ask,
    files: &[SentFile],
    in_chat: bool,
) -> Result<ItemNumber> {
    let number = item_numbers::next(tx, space.id())?;
    let (workspace_id, chat_id) = match space {
        PresentSpace::Workspace(workspace) => (Some(&workspace.id), None),
        PresentSpace::Chat(chat) => (None, Some(&chat.id)),
    };
    tx.execute(
        "INSERT INTO questions(space_number,number,workspace_id,chat_id,created_at,question,
                recommendation,reason,choices,in_chat,files)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            number.space,
            number.item,
            workspace_id,
            chat_id,
            crate::time::UnixTime::now(),
            ask.question,
            ask.recommendation,
            ask.reason,
            serde_json::to_string(&ask.choices)?,
            in_chat,
            workspace_files::sent_ids(files)?
        ],
    )?;
    Ok(number)
}

fn open_question(tx: &Transaction<'_>, number: ItemNumber) -> Result<Question> {
    let question = known(tx, number)?;
    question.ensure_open()?;
    Ok(question)
}

/// Persist `status` as the status of `question`.
fn store_status(
    tx: &Transaction<'_>,
    question: &mut Question,
    status: QuestionStatus,
) -> Result<()> {
    store_status_where(
        tx,
        &status,
        "q.space_number=:space AND q.number=:item",
        named_params! {":space": question.number.space, ":item": question.number.item},
    )?;
    question.status = status;
    Ok(())
}

/// Persist `status` on the questions `filter` keeps, with the named
/// parameters `filter_params`, without reading them; their numbers.
fn store_status_where(
    tx: &Transaction<'_>,
    status: &QuestionStatus,
    filter: &str,
    filter_params: &[(&str, &dyn rusqlite::ToSql)],
) -> Result<Vec<ItemNumber>> {
    let StatusColumns {
        answer,
        answered_at,
        message_id,
        closed_at,
        withdrawn,
    } = StatusColumns::from(status);
    let mut values = named_params! {
        ":answer": answer,
        ":answered_at": answered_at,
        ":message_id": message_id,
        ":closed_at": closed_at,
        ":withdrawn": withdrawn,
    }
    .to_vec();
    values.extend_from_slice(filter_params);
    let numbers = tx
        .prepare(&format!(
            "UPDATE questions AS q
                SET answer=:answer,answered_at=:answered_at,message_id=:message_id,
                    closed_at=:closed_at,withdrawn=:withdrawn
                WHERE {filter} RETURNING space_number,number"
        ))?
        .query_map(values.as_slice(), |row| {
            Ok(ItemNumber {
                space: row.get(0)?,
                item: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(numbers)
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
    number: ItemNumber,
    answer: &Answer,
    shown: Option<&Question>,
) -> Result<Question> {
    let mut question = open_question(tx, number)?;
    crate::refusal::ensure_unchanged(
        shown.is_none_or(|shown| question.is_same(shown)),
        format_args!("question {number}"),
    )?;
    let answer = answer.resolve(&question.ask)?;
    let message = question.answer_message(&answer);
    let status = QuestionStatus::Answered {
        answer,
        answered_at: crate::time::UnixTime::now(),
        message_id: queue_answer(tx, &question, &message)?,
    };
    store_status(tx, &mut question, status)?;
    Ok(question)
}

/// Queue `message`, which answers `question`, for the agents of its space:
/// its ID, or `None` when an identical message is already unread.
fn queue_answer(tx: &Transaction<'_>, question: &Question, message: &str) -> Result<Option<i64>> {
    let asked_by_chat: bool = tx.query_row(
        "SELECT chat_id IS NOT NULL FROM questions WHERE space_number=?1 AND number=?2",
        params![question.number.space, question.number.item],
        |row| row.get(0),
    )?;
    if asked_by_chat {
        chat_messages::insert_from_user(tx, &question.space_id, message)
    } else {
        agent_messages::insert(tx, &question.space_id, message, &[])
    }
}

/// How many questions of the workspace or chat `space_id` are open.
pub(crate) fn open_count(db: &Connection, space_id: &SpaceId) -> Result<usize> {
    let count: i64 = db.query_row(
        &format!("SELECT COUNT(*) FROM questions q WHERE {SPACE}=?1 AND {OPEN}"),
        [space_id],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count)?)
}

/// Close the space's open questions, which its finished assignment no
/// longer waits on, and return their numbers.
pub(crate) fn close_open(tx: &Transaction<'_>, space_id: &SpaceId) -> Result<Vec<ItemNumber>> {
    let closed = QuestionStatus::Closed {
        closed_at: crate::time::UnixTime::now(),
    };
    let filter = format!("{SPACE}=:space_id AND {OPEN}");
    let mut numbers =
        store_status_where(tx, &closed, &filter, named_params! {":space_id": space_id})?;
    numbers.sort_unstable();
    Ok(numbers)
}

/// Close the space's open question `number` as withdrawn by its agent.
fn withdraw(
    tx: &Transaction<'_>,
    space: &PresentSpace,
    number: ItemNumber,
    shown: Option<&Question>,
) -> Result<Question> {
    let mut question = by_number(tx, number)?
        .filter(|question| question.space_id == *space.id())
        .ok_or_else(|| {
            Refusal::new(
                Reason::NotFound,
                format!("{} has no question {number}", space.name()),
            )
        })?;
    question.ensure_open()?;
    crate::refusal::ensure_unchanged(
        shown.is_none_or(|shown| question.number == shown.number && question.is_same(shown)),
        format_args!("question {number}"),
    )?;
    let withdrawn_at = crate::time::UnixTime::now();
    store_status(
        tx,
        &mut question,
        QuestionStatus::Withdrawn { withdrawn_at },
    )?;
    Ok(question)
}

/// Who asks a question.
enum Asker {
    /// A `questions ask` command, which states it.
    Command,
    /// An agent, in its conversation, in the turn it marked.
    Chat(Turn),
}

/// What recording a question did.
enum Recorded {
    /// Nothing: the chat asker's turn has no mark.
    Unmarked,
    /// Found the same question asked before.
    Repeated(Question),
    /// Recorded a new question.
    New(Question),
}

/// What a question asks, with the files sent with it.
struct Asking {
    ask: Ask,
    files: Vec<SentFile>,
}

/// Record `asking` from `asker` in `space` unless the same question was
/// asked before. A chat asker's turn must be marked, and recording consumes
/// the mark, also for a repeated question.
fn record(
    tx: &Transaction<'_>,
    space: &PresentSpace,
    asking: &Asking,
    asker: &Asker,
    removing: &crate::daemon::chats::Removing,
) -> Result<Recorded> {
    let Asking { ask, files } = asking;
    // A space reserved for removal takes no question its cleanup checks have
    // not seen. A chat's reservation precedes the removal's next read here,
    // on the store's one thread, so the removal sees any question before it.
    match space {
        PresentSpace::Workspace(workspace) => {
            crate::daemon::store::require_ready(tx, &workspace.id)?
        }
        PresentSpace::Chat(chat) => ensure!(
            !removing.reserved(&chat.id),
            "chat {} is being removed",
            chat.name
        ),
    }
    if let Asker::Chat(turn) = asker
        && !set_chat_mark(tx, turn, false)?
    {
        return Ok(Recorded::Unmarked);
    }
    if let Some(question) = repeated(tx, space.id(), ask, files)? {
        return Ok(Recorded::Repeated(question));
    }
    let in_chat = matches!(asker, Asker::Chat(_));
    let number = insert(tx, space, ask, files, in_chat)?;
    let question = by_number(tx, number)?.context("recorded question is missing")?;
    Ok(Recorded::New(question))
}

/// The turn of an agent: the current one of the agent an execution runs, or
/// of a chat's agent, which no execution tracks.
#[derive(Debug, Clone)]
pub(crate) enum Turn {
    Execution(String),
    Chat(SpaceId),
}

/// Mark or unmark `turn` as asking in the agent's conversation; whether that
/// changed the mark.
pub(crate) fn set_chat_mark(db: &Connection, turn: &Turn, marked: bool) -> Result<bool> {
    let (table, id) = match turn {
        Turn::Execution(id) => ("executions", id.as_str()),
        Turn::Chat(id) => ("chats", id.as_str()),
    };
    let changed = db.execute(
        &format!("UPDATE {table} SET asks_in_chat=?2 WHERE id=?1 AND asks_in_chat<>?2"),
        params![id, marked],
    )?;
    Ok(changed > 0)
}

/// Close the open questions the space's agents asked in their conversations,
/// which the user replied to there, as withdrawn, retracting their unread
/// notifications; each with whether there was one to retract.
fn close_in_chat(tx: &Transaction<'_>, space_id: &SpaceId) -> Result<Vec<(Question, bool)>> {
    let open = query(
        tx,
        &format!("WHERE {SPACE}=?1 AND q.in_chat AND {OPEN}"),
        [space_id],
    )?;
    let withdrawn_at = crate::time::UnixTime::now();
    open.into_iter()
        .map(|mut question| {
            store_status(
                tx,
                &mut question,
                QuestionStatus::Withdrawn { withdrawn_at },
            )?;
            let retracted = notifications::retract(
                tx,
                space_id,
                NotificationKind::QuestionAsked,
                &question.notification(),
            )?;
            Ok((question, retracted))
        })
        .collect()
}

/// Delete the question whose answer `message_id` carried to the space
/// `space_id`, once the message is delivered.
pub(crate) fn delete_delivered(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    message_id: i64,
) -> Result<()> {
    tx.execute(
        "DELETE FROM questions WHERE COALESCE(workspace_id,chat_id)=?1 AND message_id=?2",
        params![space_id, message_id],
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
    /// Record the question with copies of the files `attachments`, which
    /// the space's attached files keep as attached by `attacher` for as long
    /// as its results, or return the same one asked again, and tell the user
    /// about a new one.
    pub async fn ask_question(
        &self,
        selector: &SpaceSelector,
        ask: Ask,
        attachments: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Question> {
        ask.validate()?;
        workspace_files::check_sent(&attachments)?;
        let space = self.space(selector).await?;
        let question = ask.question.clone();
        let asked = (self.send_with_files(
            &space.clone().into(),
            FileSource::Question,
            &question,
            attachments,
            attacher,
            |files| self.record_question(space, Asking { ask, files }, Asker::Command),
        ))
        .await?;
        asked.context("a question asked on the command line is always recorded")
    }

    /// Record the question in its turn's final `message` when the agent
    /// marked `turn` as asking in its conversation, consuming the mark, and
    /// tell the user about it.
    pub(crate) async fn ask_in_chat(
        &self,
        selector: &SpaceSelector,
        turn: Turn,
        message: Option<&str>,
    ) -> Result<Option<Question>> {
        let space = self.space(selector).await?;
        let asking = Asking {
            ask: Ask::in_chat(message),
            files: Vec::new(),
        };
        self.record_question(space, asking, Asker::Chat(turn)).await
    }

    /// Record `asking` in `space` from `asker`, or return the same question
    /// asked again, and tell the user about a new one; `None` for a chat
    /// asker whose turn has no mark.
    async fn record_question(
        &self,
        space: PresentSpace,
        asking: Asking,
        asker: Asker,
    ) -> Result<Option<Question>> {
        let removing = self.chat_removals.clone();
        let recorded = self
            .store
            .write(move |tx| record(tx, &space, &asking, &asker, &removing))
            .await?;
        match recorded {
            Recorded::Unmarked => Ok(None),
            Recorded::Repeated(question) => Ok(Some(question)),
            Recorded::New(question) => {
                self.announce_question(&question).await;
                Ok(Some(question))
            }
        }
    }

    /// Tell the user about `question` unless it was withdrawn or answered
    /// since it was asked.
    async fn announce_question(&self, question: &Question) {
        let _gate = self.gates.lock_announcements().await;
        let number = question.number;
        let stored = self.store.run(move |db| by_number(db, number)).await;
        match stored {
            Ok(Some(stored)) if stored.is_same(question) && stored.is_open() => {}
            Ok(_) => return,
            Err(error) => crate::daemon::log!("question {number} not rechecked: {error:#}"),
        }
        let about = About::space(&question.space, &question.space_id);
        let kind = NotificationKind::QuestionAsked;
        (self.notify_with_files(about, kind, question.notification(), &question.files)).await;
    }

    /// The newest open questions that fit one response, oldest first, of
    /// every space unless `selector` names one.
    pub async fn questions(&self, selector: Option<&SpaceSelector>) -> Result<Listing> {
        let owner = match selector {
            Some(selector) => Some(self.asking_space(selector).await?),
            None => None,
        };
        let questions = self
            .store
            .run(move |db| {
                query(
                    db,
                    &format!("WHERE (?1 IS NULL OR {SPACE}=?1) AND {OPEN}"),
                    [owner],
                )
            })
            .await?;
        newest_that_fit(questions)
    }

    /// The ID of the workspace or chat `selector` names, whose questions a
    /// listing keeps: a stopped chat keeps its open questions too.
    async fn asking_space(&self, selector: &SpaceSelector) -> Result<SpaceId> {
        Ok(self.space(selector).await?.id().clone())
    }

    /// Every open question, oldest first, which subscription snapshots page.
    pub(crate) async fn open_questions(&self) -> Result<Vec<Question>> {
        self.store
            .run(|db| query(db, &format!("WHERE {OPEN}"), []))
            .await
    }

    pub async fn question(&self, number: ItemNumber) -> Result<Question> {
        self.store.run(move |db| known(db, number)).await
    }

    pub async fn has_open_questions(&self, space_id: &SpaceId) -> Result<bool> {
        let id = space_id.to_owned();
        self.store
            .run(move |db| {
                crate::daemon::store::exists(
                    db,
                    &format!(
                        "SELECT 1 FROM questions q
                            WHERE (q.workspace_id=?1 OR q.chat_id=?1) AND {OPEN}"
                    ),
                    [id],
                )
            })
            .await
    }

    /// Record the user's answer to question `number`, or to `shown` when the
    /// user chose it from a listing, and queue it for the workspace's agents.
    pub async fn answer_question(
        &self,
        number: ItemNumber,
        answer: String,
        shown: Option<Question>,
    ) -> Result<Question> {
        self.record_question_answer(number, Answer::Text(answer), shown)
            .await
    }

    pub async fn accept_question_recommendation(
        &self,
        number: ItemNumber,
        shown: Option<Question>,
    ) -> Result<Question> {
        self.record_question_answer(number, Answer::Recommendation, shown)
            .await
    }

    async fn record_question_answer(
        &self,
        number: ItemNumber,
        answer: Answer,
        shown: Option<Question>,
    ) -> Result<Question> {
        let question = self
            .store
            .write(move |tx| record_answer(tx, number, &answer, shown.as_ref()))
            .await?;
        // The answer waits in the inbox of a workspace's or chat's agents.
        self.signals.agent_message_queued();
        self.signals.chat_message_queued();
        self.signals.question_changed();
        Ok(question)
    }

    /// Withdraw the space's open question `number`, which its agents no
    /// longer need answered. Its notification goes while unread; once shown,
    /// the user or the coordinating agent is told instead.
    pub async fn withdraw_question(
        &self,
        selector: &SpaceSelector,
        number: ItemNumber,
        shown: Option<Question>,
    ) -> Result<Question> {
        let space = self.space(selector).await?;
        let _gate = self.gates.lock_announcements().await;
        let (question, retracted) = self
            .store
            .write(move |tx| {
                let question = withdraw(tx, &space, number, shown.as_ref())?;
                let retracted = notifications::retract(
                    tx,
                    &question.space_id,
                    NotificationKind::QuestionAsked,
                    &question.notification(),
                )?;
                Ok((question, retracted))
            })
            .await?;
        self.signals.question_changed();
        if !retracted {
            self.notify_about(
                About::space(&question.space, &question.space_id),
                NotificationKind::QuestionWithdrawn,
                question.withdrawal_notification(),
            )
            .await;
        }
        Ok(question)
    }

    /// Close the open questions the agents of the space `selector` names
    /// asked in their conversations, which the user replied to there, with
    /// the mark of `turn`, and return their numbers.
    pub(crate) async fn close_chat_questions(
        &self,
        selector: &SpaceSelector,
        turn: Option<Turn>,
    ) -> Result<Vec<ItemNumber>> {
        let space = self.space(selector).await?;
        let _gate = self.gates.lock_announcements().await;
        let closed = self
            .store
            .write(move |tx| {
                if let Some(turn) = &turn {
                    set_chat_mark(tx, turn, false)?;
                }
                let closed = close_in_chat(tx, space.id())?;
                Ok(closed)
            })
            .await?;
        if !closed.is_empty() {
            self.signals.question_changed();
        }
        // As for a withdrawal, whoever saw the question, the user or the
        // coordinating agent it went to instead, is told it closed.
        for (question, _) in closed.iter().filter(|(_, retracted)| !retracted) {
            self.notify_about(
                About::space(&question.space, &question.space_id),
                NotificationKind::QuestionWithdrawn,
                question.withdrawal_notification(),
            )
            .await;
        }
        Ok(closed.iter().map(|(question, _)| question.number).collect())
    }

    /// Return the space's question `item` asking `ask` once it is answered.
    /// What it asks tells it from a later question reusing the number; a
    /// later identical question asks the same, so its answer serves the wait
    /// too.
    pub async fn await_answer(
        &self,
        selector: &SpaceSelector,
        item: u32,
        ask: &Ask,
    ) -> Result<Question> {
        let space = self.space(selector).await?;
        let number = ItemNumber {
            space: space.number(),
            item,
        };
        // Subscribe before reading, so an answer in between still wakes.
        let mut changed = self.signals.watch_questions();
        loop {
            let question = self
                .store
                .run(move |db| by_number(db, number))
                .await?
                .filter(|question| question.space_id == *space.id() && question.ask == *ask)
                .with_context(|| {
                    format!(
                        "{} has no question {number} awaiting delivery; a delivered answer removes its question",
                        space.name()
                    )
                })?;
            if let QuestionStatus::Answered { .. } = question.status {
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
