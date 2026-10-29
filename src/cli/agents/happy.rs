//! Happy runs an agent in a detached session the Happy app shows, started
//! as Happy's own daemon starts them, and delivers a first message the agent
//! cannot take as an argument through Happy's server.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;
use serde_json::json;

use super::{
    Integration, Options,
    detached::{App, Delivered, Prepared, Session},
    trust::TrustFile,
};
use crate::{
    ai::IntegrationKind,
    config,
    happy::{self, Agent},
};

/// `happy-<agent>`: the agent in a detached Happy session.
pub(super) struct Happy(Agent);

pub(super) const CLAUDE: Happy = Happy(Agent::Claude);
pub(super) const CODEX: Happy = Happy(Agent::Codex);

impl Happy {
    /// The integration of the agent Happy runs.
    fn inner(&self) -> &'static dyn Integration {
        super::integration(match self.0 {
            Agent::Claude => IntegrationKind::Claude,
            Agent::Codex => IntegrationKind::Codex,
        })
    }
}

impl Integration for Happy {
    /// Happy needs both its own CLI and the agent it runs.
    fn programs(&self) -> Option<Vec<&'static str>> {
        Some(vec!["happy", self.0.as_str()])
    }

    fn trust_file(&self) -> Option<&'static TrustFile> {
        self.inner().trust_file()
    }

    fn detached(&self) -> Option<&dyn App> {
        Some(self)
    }
}

impl App for Happy {
    fn name(&self) -> &'static str {
        "Happy"
    }

    fn agent(&self) -> &'static str {
        self.0.as_str()
    }

    fn execution_agent(&self) -> String {
        format!("happy {}", self.0.as_str())
    }

    fn place(&self) -> &'static str {
        "the Happy app"
    }

    /// Happy's Codex mode takes no arguments.
    fn takes_agent_args(&self) -> bool {
        happy::accepts_prompt(self.0)
    }

    /// A launch from inside another Happy session must not attach to it.
    fn not_inherited(&self) -> &'static [&'static str] {
        &happy::client::RECONNECT_ENV
    }

    /// Run the session the way Happy's own daemon does, so it registers with
    /// that daemon and appears in the app.
    fn prepare<'a>(&'a self, session: Session<'a>) -> BoxFuture<'a, Result<Box<dyn Prepared>>> {
        Box::pin(async move {
            let daemon_recorded = warn_without_daemon(&session.paths.home);
            let opening = Opening::prepare(self.0, &session).await?;
            let command = happy::command(self.0, session.prompt.as_deref(), session.args);
            Ok(Box::new(HappySession {
                command,
                prompt: session.prompt,
                opening,
                daemon_recorded,
            }) as Box<dyn Prepared>)
        })
    }
}

/// What Happy's section sets: `happy.enabled` turns Happy sessions on.
pub(super) fn options(config: &config::Happy) -> Options {
    Options {
        off: (!config.enabled).then_some(
            "Happy sessions are off; set happy.enabled = true in the repository or global config",
        ),
        ..Options::default()
    }
}

/// A Happy session ready to start.
struct HappySession {
    command: Vec<OsString>,
    prompt: Option<String>,
    opening: Opening,
    /// Whether Happy's daemon had recorded itself.
    daemon_recorded: bool,
}

impl Prepared for HappySession {
    fn command(&self) -> Vec<OsString> {
        self.command.clone()
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        self.opening.env()
    }

    fn discard(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(self.opening.discard())
    }

    fn deliver(self: Box<Self>) -> BoxFuture<'static, Delivered> {
        Box::pin(async move {
            let prompt_delivered = self.opening.deliver(self.prompt.as_deref()).await;
            let fields = json!({
                "prompt_file": self.opening.prompt_file,
                "prompt_delivered": prompt_delivered,
                "happy_session_id": self.opening.seeded.as_ref().map(|seeded| seeded.session.id.clone()),
                "happy_daemon_recorded": self.daemon_recorded,
            });
            Delivered {
                note: self.opening.delivery(prompt_delivered),
                fields,
            }
        })
    }
}

/// Whether Happy's daemon has recorded itself under `home`, warning when it
/// has not, since its sessions appear in the app only once it runs.
fn warn_without_daemon(home: &Path) -> bool {
    let daemon_state = happy::daemon_state_path(home);
    let recorded = daemon_state.is_file();
    if !recorded {
        eprintln!(
            "warning: Happy daemon state not found at {}; the session will not appear in the Happy app until `happy daemon start` runs",
            daemon_state.display()
        );
    }
    recorded
}

/// Keep `prompt` beside the session's other `files`, for the user to send
/// when Happy cannot deliver it.
fn save_prompt(files: &Path, prompt: &str) -> Result<PathBuf> {
    let path = files.with_extension(crate::paths::SESSION_PROMPT);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, prompt).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// How a session's first message reaches it. Happy's Codex mode has no prompt
/// argument: the prompt is kept in a file and, when this machine is logged in
/// to Happy, delivered through Happy's server to a session Shoal creates for
/// the agent to attach to.
struct Opening {
    prompt_file: Option<PathBuf>,
    seeded: Option<happy::client::Seeded>,
}

impl Opening {
    /// Keep the prompt beside the session's other `files` and seed its
    /// session, for an agent that cannot take it as an argument.
    async fn prepare(agent: Agent, session: &Session<'_>) -> Result<Self> {
        let Session {
            paths,
            directory,
            branch,
            files,
            prompt,
            ..
        } = session;
        let Some(prompt) = prompt.as_deref().filter(|_| !happy::accepts_prompt(agent)) else {
            return Ok(Self {
                prompt_file: None,
                seeded: None,
            });
        };
        let path = save_prompt(files, prompt)?;
        let seeded = match happy::client::seed(paths, directory, *branch, agent).await {
            Ok(seeded) => Some(seeded),
            Err(error) => {
                eprintln!(
                    "warning: cannot deliver the prompt through Happy ({error:#}); send the prompt saved at {} to the session yourself",
                    path.display()
                );
                None
            }
        };
        Ok(Self {
            prompt_file: Some(path),
            seeded,
        })
    }

    /// The variables that attach the session to the seeded one.
    fn env(&self) -> Vec<(&'static str, String)> {
        self.seeded
            .as_ref()
            .map(|seeded| seeded.session.env())
            .unwrap_or_default()
    }

    /// Nothing will ever attach to the seeded session; do not leave it in the app.
    async fn discard(self) {
        if let Some(seeded) = self.seeded {
            seeded.discard().await;
        }
    }

    /// Send `prompt` to the started session, returning whether it arrived. A
    /// prompt passed as an argument is delivered by the launch itself.
    async fn deliver(&self, prompt: Option<&str>) -> bool {
        let Some(prompt) = prompt else {
            return false;
        };
        let (Some(seeded), Some(path)) = (&self.seeded, &self.prompt_file) else {
            return self.prompt_file.is_none();
        };
        match seeded
            .deliver(prompt, std::time::Duration::from_secs(90))
            .await
        {
            Ok(()) => true,
            Err(error) => {
                eprintln!(
                    "warning: prompt not delivered ({error:#}); send the prompt saved at {} to the session yourself",
                    path.display()
                );
                false
            }
        }
    }

    /// The line reporting where a saved prompt went.
    fn delivery(&self, delivered: bool) -> &'static str {
        match (&self.prompt_file, delivered) {
            (Some(_), true) => "\nPrompt delivered to the session",
            (Some(_), false) => "\nPrompt saved for you to send from the app",
            (None, _) => "",
        }
    }
}
