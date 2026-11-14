//! Running the gate on one request: once the agent queue admits it, as a
//! one-shot run in the repository's checkout that resumes its subject's
//! session or starts one, once more told why when its escalation quotes no
//! listed case, then once by the fallback agent when it fails.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::watch;

use super::{
    context::Case,
    prompt,
    store::{self, Session},
    verdict::{self, Invalid, Verdict},
};
use crate::{
    agent::{Agent, AgentSessionId},
    daemon::{
        approvals::ApprovalRequest,
        log, oneshot,
        workspace::{
            Manager,
            agent_queue::{self, OneShotAdmission, Serving},
        },
    },
    reviews::{FailureReason, output},
    shoal_dirs::{Contents, ShoalDir},
};

/// A run still running after this long is stopped; a decision that long
/// has stopped making progress.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The most of a run's result that is read.
const RESULT_LIMIT: u64 = 1024 * 1024;

/// The most of each diagnostic stream that is read, from its end, and of
/// standard error from its start, where a tool names its session.
const DIAGNOSTIC_LIMIT: u64 = 64 * 1024;

/// What the gate made of a request.
pub(super) enum Consulted {
    Decided {
        verdict: Verdict,
        agent: Agent,
        /// The session to keep for the subject: the one the case's agent
        /// reported; a fallback's is not kept.
        session: Option<AgentSessionId>,
    },
    /// Neither agent returned a verdict, for this reason.
    Failed(String),
}

/// What one run returned.
struct Returned {
    result: Value,
    session: Option<AgentSessionId>,
}

/// The prompts of one request: what a resumed session is asked, the whole
/// text a new session is given, and the session to resume.
struct Prompts {
    /// The whole text a new session is given, with every commit of the
    /// change.
    full: String,
    /// The subject's session to resume, and what it is asked: only the
    /// request, the commits since its last verdict and the space's issues
    /// and PRs as they are now.
    resumed: Option<(AgentSessionId, String)>,
}

impl Manager {
    /// Ask the gate about `request`, as its case describes it: its own
    /// agent, then its fallback once when that fails.
    pub(super) async fn consult_gate(&self, request: &ApprovalRequest, case: &Case) -> Consulted {
        let prompts = self.gate_prompts(request, case).await;
        let failure = match self.ask_gate_agent(request, case, &prompts).await {
            Ok(decided) => return decided,
            Err(failure) => failure,
        };
        match &case.fallback {
            Some(fallback) => {
                (self.ask_fallback(request, case, fallback, &prompts.full, failure)).await
            }
            None => Consulted::Failed(format!("{}: {}", case.agent, described(failure))),
        }
    }

    /// The prompts of `request`: a new session's, and its subject's
    /// resumable session's when it has one.
    async fn gate_prompts(&self, request: &ApprovalRequest, case: &Case) -> Prompts {
        let commits = self.gate_commits(case, None).await;
        let opening = request_prompt(request, case, commits.as_deref(), None);
        let resumed = match self.resumable_gate_session(request, case).await {
            Some(saved) => {
                let since = self.gate_commits(case, saved.seen.as_deref()).await;
                let asked = request_prompt(request, case, since.as_deref(), Some(&case.items));
                Some((saved.session, asked))
            }
            None => None,
        };
        Prompts {
            full: format!("{}\n\n{opening}", case.opening),
            resumed,
        }
    }

    /// The case's own agent's verdict, resuming the subject's session when
    /// there is one, and in a new session when there is none or resuming it
    /// failed for a reason other than running too long; or why it returned
    /// none.
    async fn ask_gate_agent(
        &self,
        request: &ApprovalRequest,
        case: &Case,
        prompts: &Prompts,
    ) -> Result<Consulted, FailureReason> {
        if let Some((session, asked)) = &prompts.resumed {
            match self
                .ask_gate(request, case, &case.agent, Some(session), asked)
                .await
            {
                Ok(returned) => {
                    let resumed = Some(session.clone());
                    return (self.read_verdict(request, case, returned, resumed, &prompts.full))
                        .await;
                }
                Err(FailureReason::TimedOut) => return Err(FailureReason::TimedOut),
                Err(failure) => log!(
                    "approval {}: resuming the gate's session failed ({failure}); it starts a new one",
                    request.number
                ),
            }
        }
        let returned = (self.ask_gate(request, case, &case.agent, None, &prompts.full)).await?;
        (self.read_verdict(request, case, returned, None, &prompts.full)).await
    }

    /// The verdict of `fallback`, in a new run of the `full` prompt, after
    /// the case's agent failed for `failure`.
    async fn ask_fallback(
        &self,
        request: &ApprovalRequest,
        case: &Case,
        fallback: &Agent,
        full: &str,
        failure: FailureReason,
    ) -> Consulted {
        log!(
            "approval {}: the gate {} failed ({failure}); {fallback} decides instead",
            request.number,
            case.agent
        );
        let returned = self.ask_gate(request, case, fallback, None, full).await;
        let verdict = returned.and_then(|returned| {
            verdict::read(returned.result, &case.rules.escalate)
                .map_err(|_| FailureReason::InvalidOutput)
        });
        match verdict {
            Ok(verdict) => Consulted::Decided {
                verdict,
                agent: fallback.clone(),
                session: None,
            },
            Err(reason) => Consulted::Failed(format!(
                "{}: {}; then {fallback}: {}",
                case.agent,
                described(failure),
                described(reason)
            )),
        }
    }

    /// The verdict `returned` holds; one whose escalation quotes no listed
    /// case asks the gate once more, told why, in the session it reported
    /// or `resumed`, else in a new run of the `full` prompt.
    async fn read_verdict(
        &self,
        request: &ApprovalRequest,
        case: &Case,
        returned: Returned,
        resumed: Option<AgentSessionId>,
        full: &str,
    ) -> Result<Consulted, FailureReason> {
        let session = returned.session.or(resumed);
        let quoted = match verdict::read(returned.result, &case.rules.escalate) {
            Ok(verdict) => {
                let agent = case.agent.clone();
                return Ok(Consulted::Decided {
                    verdict,
                    agent,
                    session,
                });
            }
            Err(Invalid::Malformed) => return Err(FailureReason::InvalidOutput),
            Err(Invalid::Misquoted(quoted)) => quoted,
        };
        let told = prompt::misquoted(&quoted);
        let again = match &session {
            Some(session) => {
                (self.ask_gate(request, case, &case.agent, Some(session), &told)).await?
            }
            None => {
                let prompt = format!("{full}\n\n{told}");
                self.ask_gate(request, case, &case.agent, None, &prompt)
                    .await?
            }
        };
        let verdict = verdict::read(again.result, &case.rules.escalate)
            .map_err(|_| FailureReason::InvalidOutput)?;
        Ok(Consulted::Decided {
            verdict,
            agent: case.agent.clone(),
            session: again.session.or(session),
        })
    }

    /// The subject's saved session, when the case's agent opened it with the
    /// same rules and can resume it.
    async fn resumable_gate_session(
        &self,
        request: &ApprovalRequest,
        case: &Case,
    ) -> Option<Session> {
        let resumes = (case.settings.tools.get(case.agent.name()))
            .is_some_and(|tool| tool.launch.arguments.review_resume.is_some());
        if !resumes {
            return None;
        }
        let (space, subject) = (request.space_id.clone(), case.subject.key());
        let saved = self
            .store
            .read(move |tx| store::session(tx, &space, &subject))
            .await;
        match saved {
            Ok(saved) => {
                saved.filter(|saved| saved.agent == case.agent && saved.rules == case.opened_with)
            }
            Err(error) => {
                log!("approval {}: the gate's session: {error:#}", request.number);
                None
            }
        }
    }

    /// One run of `agent` with `prompt`, resuming `session` when given, once
    /// the agent queue admits it; protection that stops it runs it again
    /// once load recovers.
    async fn ask_gate(
        &self,
        request: &ApprovalRequest,
        case: &Case,
        agent: &Agent,
        session: Option<&AgentSessionId>,
        prompt: &str,
    ) -> Result<Returned, FailureReason> {
        let mut after_stop = false;
        loop {
            let (admission, mut cancelled) = (self.admit_gate(request, case, after_stop))
                .await
                .map_err(|error| failed(request, error))?;
            let files = run_files(case, request).map_err(|error| failed(request, error))?;
            let ended = self
                .run_gate_once(case, agent, session, prompt, &files, &mut cancelled)
                .await;
            match ended {
                Ok(Ran::Stopped) => {
                    remove_run(&files);
                    after_stop = true;
                }
                Ok(Ran::LeftRunning) => {
                    admission.keep();
                    return Err(FailureReason::ExecutionFailed);
                }
                Ok(Ran::Ended(returned)) => {
                    remove_run(&files);
                    return returned;
                }
                Err(error) => {
                    remove_run(&files);
                    return Err(failed(request, error));
                }
            }
        }
    }

    /// The agent queue's admission of a gate run for `request`, ranked with
    /// the work of its space, with the signal by which protection stops it;
    /// a run it stopped asks again `after_stop`.
    async fn admit_gate(
        &self,
        request: &ApprovalRequest,
        case: &Case,
        after_stop: bool,
    ) -> Result<(OneShotAdmission<'_>, watch::Receiver<bool>)> {
        let (cancel, cancelled) = watch::channel(false);
        let stop = Arc::new(cancel);
        let queued_after = self.store.read(|tx| agent_queue::last_seq(tx)).await?;
        let serving = Serving {
            space: request.space_id.clone(),
            queued_after,
        };
        let mut admission = (self.agent_queue).request_oneshot(
            &case.repository.id,
            serving,
            after_stop,
            move || {
                stop.send_replace(true);
            },
        );
        admission.admitted().await;
        Ok((admission, cancelled))
    }

    /// Run `agent` once in the checkout, writing to `files`.
    async fn run_gate_once(
        &self,
        case: &Case,
        agent: &Agent,
        session: Option<&AgentSessionId>,
        prompt: &str,
        files: &Path,
        cancelled: &mut watch::Receiver<bool>,
    ) -> Result<Ran> {
        let command = gate_command(case, agent, session, prompt, files, &self.paths.home)?;
        let run = oneshot::Run {
            runs: &self.oneshot_runs,
            command: &command.command,
            directory: &case.repository.path,
            files,
            timeout: TIMEOUT,
        };
        let ended = match oneshot::run(&run, cancelled).await {
            Ok(ended) => ended,
            Err(error) if error.downcast_ref::<oneshot::LeftRunning>().is_some() => {
                log!("ending the gate: {error:#}");
                return Ok(Ran::LeftRunning);
            }
            Err(error) => return Err(error),
        };
        Ok(match ended {
            oneshot::Ended::Cancelled => Ran::Stopped,
            oneshot::Ended::TimedOut => Ran::Ended(Err(FailureReason::TimedOut)),
            oneshot::Ended::Exited { success } => {
                Ran::Ended(read_run(command.format, files, &command.result, success)?)
            }
        })
    }
}

/// A gate run's command, with where and how its result comes back.
struct GateCommand {
    command: oneshot::AgentCommand,
    /// The file it writes its result to: its output file, or its standard
    /// output.
    result: PathBuf,
    format: &'static output::ResultFormat,
}

/// The command that runs `agent` once with `prompt` in a run writing to
/// `files`, resuming `session` when given, after writing the output schema
/// there.
fn gate_command(
    case: &Case,
    agent: &Agent,
    session: Option<&AgentSessionId>,
    prompt: &str,
    files: &Path,
    home: &Path,
) -> Result<GateCommand> {
    std::fs::create_dir_all(files).with_context(|| format!("create {}", files.display()))?;
    let (schema, output) = (files.join("schema.json"), files.join("result.json"));
    std::fs::write(&schema, verdict::SCHEMA)?;
    let fields = crate::ai::ReviewFields {
        prompt,
        schema: &schema,
        schema_json: verdict::SCHEMA,
        output: &output,
    };
    let tool = (case.settings.tools.get(agent.name()))
        .with_context(|| format!("no AI tool runs {agent}"))?;
    let arguments = &tool.launch.arguments;
    let args = match session {
        Some(session) => arguments.review_resume_args(&fields, session),
        None => arguments.review_args(&fields),
    }
    .with_context(|| format!("{agent} has no template for a one-shot run"))?;
    let model = case.settings.model(agent, Default::default());
    let configured = BTreeMap::new();
    Ok(GateCommand {
        command: oneshot::agent_command(&case.settings, agent, &model, args, &configured, home)?,
        result: match session.map_or(arguments.review_writes_output(), |_| {
            arguments.review_resume_writes_output()
        }) {
            true => output,
            false => oneshot::stdout(files),
        },
        format: tool.review_result,
    })
}

/// How one run went.
enum Ran {
    Ended(Result<Returned, FailureReason>),
    /// Protection stopped it.
    Stopped,
    /// What it started could not be proven stopped, so it stays tracked.
    LeftRunning,
}

/// What a run in `files` that exited with `success` returned in `format`,
/// its result read from `source`.
fn read_run(
    format: &'static output::ResultFormat,
    files: &std::path::Path,
    source: &std::path::Path,
    success: bool,
) -> Result<Result<Returned, FailureReason>> {
    let result = oneshot::read_bounded(source, RESULT_LIMIT)?;
    let stderr = oneshot::stderr(files);
    let head = oneshot::read_head(&stderr, DIAGNOSTIC_LIMIT)?;
    let tail = oneshot::read_tail(&stderr, DIAGNOSTIC_LIMIT)?;
    let stdout = oneshot::read_tail(&oneshot::stdout(files), DIAGNOSTIC_LIMIT)?;
    let run = output::Run {
        success,
        result: result.as_deref(),
        diagnostics: [&tail, &stdout],
    };
    Ok(output::result(format, &run).map(|value| Returned {
        result: value,
        session: output::session(format, result.as_deref(), &head),
    }))
}

/// A new run directory of `request`, in its repository's gate runs, one
/// per run, so a run whose processes outlived it never shares its files or
/// its processes' marker with the next.
fn run_files(case: &Case, request: &ApprovalRequest) -> Result<PathBuf> {
    let runs = ShoalDir::of(&case.repository, Contents::GateRuns)
        .context("the repository has no directory")?;
    runs.create()?;
    let run = format!("{}-{}", request.key, uuid::Uuid::new_v4().simple());
    Ok(runs.root().join(run))
}

/// Remove a run's files, which only logs when it fails.
pub(super) fn remove_run(files: &std::path::Path) {
    match std::fs::remove_dir_all(files) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            log!("remove {}: {error}", files.display());
        }
        _ => {}
    }
}

/// The request part of the gate's prompt, with `commits`, and with `items`
/// for a resumed session, whose opening listed them as they were.
fn request_prompt(
    request: &ApprovalRequest,
    case: &Case,
    commits: Option<&str>,
    items: Option<&[String]>,
) -> String {
    let specification = serde_json::to_string_pretty(&request.specification).unwrap_or_default();
    prompt::request(&prompt::Request {
        title: &format!("{}: {}", request.number, request.subject()),
        target: &request.target.to_string(),
        reason: &request.reason,
        specification: &specification,
        commits,
        warnings: &case.warnings,
        items,
    })
}

/// What `reason` says of a gate agent's failed run.
fn described(reason: FailureReason) -> String {
    let text = match reason {
        FailureReason::UsageLimit => "its usage limit was reached",
        FailureReason::QuotaExhausted => "its quota or credits are exhausted",
        FailureReason::RateLimit => "it was rate limited",
        FailureReason::AuthenticationFailed => "it could not authenticate",
        FailureReason::InvalidOutput => "its output was no verdict",
        FailureReason::TimedOut => {
            return format!(
                "it ran longer than {} minutes and was stopped",
                TIMEOUT.as_secs() / 60
            );
        }
        FailureReason::ExecutionFailed | FailureReason::InvalidFalsePositives => {
            "it exited unsuccessfully"
        }
    };
    text.to_owned()
}

/// A run that could not be prepared or read, which fails it.
fn failed(request: &ApprovalRequest, error: anyhow::Error) -> FailureReason {
    log!("approval {}: the gate: {error:#}", request.number);
    FailureReason::ExecutionFailed
}
