//! Apps that run agent sessions detached from any terminal, such as Happy,
//! in which the user continues them: what Shoal asks of such an [`App`] to
//! start a session and deliver its first message.
use std::{ffi::OsString, path::Path};

use anyhow::Result;
use futures_util::future::BoxFuture;

use crate::client::output::agents::Delivery;
use crate::{agent::ModelChoice, config::Effective, paths::Paths};

/// An app that runs agent sessions apart from any terminal.
pub(crate) trait App: Sync {
    /// The app's name in output.
    fn name(&self) -> &'static str;

    /// The agent the app runs.
    fn agent(&self) -> &'static str;

    /// What a workspace session's execution records as its agent.
    fn execution_agent(&self) -> String;

    /// Where its sessions run, for a user looking for one.
    fn place(&self) -> &'static str;

    /// Whether the agent takes the arguments it is configured with, such as
    /// its instructions apart from its first message and other working
    /// directories.
    fn takes_agent_args(&self) -> bool;

    /// Variables a session must not inherit from this process.
    fn not_inherited(&self) -> &'static [&'static str];

    /// What `session` runs and how its first message reaches it.
    fn prepare<'a>(&'a self, session: Session<'a>) -> BoxFuture<'a, Result<Box<dyn Prepared>>>;
}

/// A session an [`App`] is to run, as Shoal sets it up, which
/// integrations read.
pub(crate) struct Session<'a> {
    pub paths: &'a Paths,
    /// Where the session runs.
    pub directory: &'a Path,
    /// The branch checked out there, for a workspace's session.
    pub branch: Option<&'a str>,
    /// Where the session keeps its files, without an extension.
    pub files: &'a Path,
    /// The first message.
    pub prompt: Option<String>,
    pub args: Vec<OsString>,
}

/// A session an [`App`] prepared.
pub(crate) trait Prepared: Send + Sync {
    /// The command the session runs.
    fn command(&self) -> Vec<OsString>;

    /// Variables the session runs with.
    fn env(&self) -> Vec<(&'static str, String)>;

    /// Undo the preparation of a session that never started.
    fn discard(self: Box<Self>) -> BoxFuture<'static, ()>;

    /// Deliver the first message to the started session.
    fn deliver(self: Box<Self>) -> BoxFuture<'static, Delivered>;
}

/// What output says of a first message once the session started.
pub(crate) struct Delivered {
    /// The line the text ends with, if any.
    pub note: &'static str,
    /// What the JSON adds.
    pub fields: Delivery,
}

/// The arguments that run `app`'s agent, started as `name`, with `model`;
/// refused when the app passes the agent no arguments or the agent has no
/// template for a value `model` sets.
pub(crate) fn model_args(
    app: &dyn App,
    settings: &Effective,
    name: &str,
    model: &ModelChoice,
) -> Result<Vec<OsString>> {
    if model.is_empty() {
        return Ok(Vec::new());
    }
    anyhow::ensure!(
        app.takes_agent_args(),
        "cannot start {name} with a configured or requested model or effort: {} passes {} no \
         arguments",
        app.name(),
        app.agent()
    );
    super::model_args(settings, app.agent(), model)
}
