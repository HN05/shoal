//! What a member reaches the hub by: a WebSocket to its URL, over TLS for
//! an `https` one, or the standard input and output of a command, whose
//! standard error goes to the daemon's log.
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use shoal_protocol::hub::{HubUrl, Route};
use tokio::{
    io::{AsyncBufReadExt as _, BufReader},
    process::{Child, ChildStderr},
};
use tokio_tungstenite::Connector;

use crate::{
    log,
    transport::{self, Channel},
};

/// The lines of a command's standard error kept to explain its failure.
const KEPT_LINES: usize = 20;

/// How long a failed command has to exit before its failure is told.
const EXIT_WAIT: Duration = Duration::from_secs(5);

/// An open transport to the hub, which closes when dropped.
pub(super) struct Transport {
    channel: Option<Channel>,
    command: Option<Command>,
}

/// The command a transport runs, which it kills when dropped.
struct Command {
    program: String,
    child: Child,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl Transport {
    pub(super) async fn connect(route: &Route) -> Result<Self> {
        match route {
            Route::Url(url) => Self::websocket(url).await,
            Route::Command(command) => Self::command(command),
        }
    }

    async fn websocket(url: &HubUrl) -> Result<Self> {
        let connector = Connector::Rustls(tls());
        let connected = tokio_tungstenite::connect_async_tls_with_config(
            url.link(),
            Some(transport::websocket_limits()),
            false,
            Some(connector),
        );
        let (socket, _) = connected
            .await
            .with_context(|| format!("could not reach the hub at {url}"))?;
        Ok(Self {
            channel: Some(Channel::websocket(socket)),
            command: None,
        })
    }

    fn command(command: &[String]) -> Result<Self> {
        let (program, args) = command
            .split_first()
            .context("[machine.hub] connect names no command")?;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("could not run {program} to reach the hub"))?;
        let (input, output) = (child.stdin.take(), child.stdout.take());
        let channel = Channel::lines(
            BufReader::new(output.expect("piped")),
            input.expect("piped"),
        );
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        tokio::spawn(keep_stderr(
            child.stderr.take().expect("piped"),
            stderr.clone(),
        ));
        Ok(Self {
            channel: Some(channel),
            command: Some(Command {
                program: command.join(" "),
                child,
                stderr,
            }),
        })
    }

    pub(super) fn channel(&mut self) -> &mut Channel {
        self.channel
            .as_mut()
            .expect("a transport's channel is taken once")
    }

    /// The channel, for a session that runs while the transport is kept.
    pub(super) fn take_channel(&mut self) -> Channel {
        self.channel
            .take()
            .expect("a transport's channel is taken once")
    }

    /// `error`, which opening the link failed with, and how the command
    /// failed, with what fixes a `shoal` the remote side does not find.
    pub(super) async fn failure(&mut self, error: anyhow::Error) -> anyhow::Error {
        let Some(command) = &mut self.command else {
            return error;
        };
        drop(self.channel.take());
        let status = tokio::time::timeout(EXIT_WAIT, command.child.wait()).await;
        let stderr = command
            .stderr
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut message = format!(
            "{} did not open a link to the hub: {error:#}",
            command.program
        );
        if let Ok(Ok(status)) = status {
            message.push_str(&format!("; it exited with {status}"));
        }
        if !stderr.is_empty() {
            message.push_str(&format!(":\n{}", stderr.join("\n")));
        }
        let missing = status
            .is_ok_and(|status| status.is_ok_and(|status| status.code() == Some(127)))
            || stderr.iter().any(|line| line.contains("not found"));
        if missing {
            message.push_str(
                "\nWhere shoal is not on the PATH of the other side, name it by its absolute path, as in ssh desk /home/me/.local/bin/shoal daemon connect",
            );
        }
        anyhow!(message)
    }
}

/// Log each line of `stderr` and keep the last [`KEPT_LINES`] in `kept`,
/// each as short as the daemon keeps what another side sent.
async fn keep_stderr(stderr: ChildStderr, kept: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = crate::shown(&line);
        log!("hub connect: {line}");
        let mut kept = kept.lock().unwrap();
        if kept.len() == KEPT_LINES {
            kept.pop_front();
        }
        kept.push_back(line);
    }
}

/// The TLS settings of a link to an `https` hub: the web's root
/// certificates, through the provider the agents' proxy uses too.
fn tls() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring supports the default TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}
