//! Happy runs an agent in a detached session the Happy app shows, started
//! as Happy's own daemon starts them, and delivers a first message the agent
//! cannot take as an argument through Happy's server.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;

use super::{
    Integration,
    detached::{App, Delivered, Prepared, Session},
    trust::TrustFile,
};
use crate::client::output::agents::Delivery;
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

    fn agent(&self) -> crate::agent::Agent {
        crate::agent::Agent::unchecked(self.0.as_str().to_owned())
    }

    fn execution_agent(&self) -> crate::agent::Agent {
        crate::agent::Agent::unchecked(format!("happy {}", self.0.as_str()))
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

/// Why Happy sessions are off under `settings`: they are until
/// `agents.happy.enabled` turns them on.
pub(super) fn off(settings: &config::Effective) -> Option<&'static str> {
    let enabled = settings.agents.get("happy").and_then(|agent| agent.enabled);
    (enabled != Some(true)).then_some(
        "Happy sessions are off; set agents.happy.enabled = true in the repository or global config",
    )
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

    fn deliver(&self) -> BoxFuture<'_, Result<Delivered>> {
        Box::pin(async move {
            self.opening.deliver(self.prompt.as_deref()).await?;
            let fields = Delivery {
                happy_session_id: self
                    .opening
                    .seeded
                    .as_ref()
                    .map(|seeded| seeded.session.id.clone()),
                happy_daemon_recorded: Some(self.daemon_recorded),
            };
            Ok(Delivered {
                note: self.opening.note(),
                fields,
            })
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

/// How a session's first message reaches it. Happy's Codex mode has no prompt
/// argument: the prompt is delivered through Happy's server to a session
/// Shoal creates for the agent to attach to, which needs this machine logged
/// in to Happy.
struct Opening {
    seeded: Option<happy::client::Seeded>,
}

impl Opening {
    /// Seed the session of an agent that cannot take its first message as
    /// an argument, refusing the launch when Happy cannot take it.
    async fn prepare(agent: Agent, session: &Session<'_>) -> Result<Self> {
        let Session {
            paths,
            directory,
            branch,
            prompt,
            ..
        } = session;
        if prompt.is_none() || happy::accepts_prompt(agent) {
            return Ok(Self { seeded: None });
        }
        let seeded = happy::client::seed(paths, directory, *branch, agent)
            .await
            .with_context(|| {
                format!(
                    "cannot start happy-{}: it takes its first message only through Happy's \
                     server, which needs this machine logged in to Happy",
                    agent.as_str()
                )
            })?;
        Ok(Self {
            seeded: Some(seeded),
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

    /// Send `prompt` to the started session through its seeded one. A
    /// prompt passed as an argument is delivered by the launch itself.
    async fn deliver(&self, prompt: Option<&str>) -> Result<()> {
        let (Some(seeded), Some(prompt)) = (&self.seeded, prompt) else {
            return Ok(());
        };
        seeded
            .deliver(prompt, std::time::Duration::from_secs(90))
            .await
            .context("the Happy session did not receive its first message")
    }

    /// The line reporting a prompt delivered through the seeded session.
    fn note(&self) -> &'static str {
        match self.seeded {
            Some(_) => "\nPrompt delivered to the session",
            None => "",
        }
    }
}
