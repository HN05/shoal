//! How the daemon reads the values of its settings: a command another
//! machine of the hub runs goes there through the hub, and the last value
//! each such command printed stays in memory, never on disk, for while that
//! machine cannot be reached.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

use anyhow::{Result, anyhow};
use futures_util::future::BoxFuture;

use super::value::{self, Value};

pub enum Unanswered {
    /// The machine, or the hub on the way to it, cannot be reached now.
    Unreachable(anyhow::Error),
    /// The machine answered, refusing the request or failing to run the
    /// command.
    Failed(anyhow::Error),
}

/// The daemon's way to the other machines of its hub.
pub trait Remote: Send + Sync {
    /// What `command` prints on the machine `machine` names, run for the
    /// setting `setting`.
    fn run<'a>(
        &'a self,
        machine: &'a str,
        setting: &'a str,
        command: &'a [String],
    ) -> BoxFuture<'a, Result<String, Unanswered>>;
}

/// A command and the machine that runs it, which a kept value answers.
type Source = (Vec<String>, String);

#[derive(Default)]
pub struct Values {
    remote: RwLock<Option<Arc<dyn Remote>>>,
    kept: Mutex<HashMap<Source, String>>,
}

impl Values {
    /// Reach other machines through `remote` from now on.
    pub fn connect(&self, remote: Arc<dyn Remote>) {
        *self.remote.write().unwrap() = Some(remote);
    }

    /// The value `value` gives the setting `setting`: a command another
    /// machine runs is fetched there, or while that machine cannot be
    /// reached taken from the last value it printed; a command of this
    /// machine runs bounded, and a variable is the daemon's.
    pub async fn resolve(&self, setting: &str, value: &Value) -> Result<String> {
        match value {
            Value::Cmd {
                cmd,
                on: Some(machine),
            } => self.fetch(setting, cmd, machine).await,
            Value::Cmd { cmd, on: None } => value::bounded_output(cmd).await,
            value => value::resolve(value, |name| std::env::var_os(name)).await,
        }
    }

    /// What `command` prints for `setting` on `machine`, kept for when that
    /// machine cannot be reached.
    async fn fetch(&self, setting: &str, command: &[String], machine: &str) -> Result<String> {
        let remote = self.remote.read().unwrap().clone().ok_or_else(|| {
            anyhow!("{setting} runs on {machine}, but this machine is joined to no hub")
        })?;
        let source = (command.to_vec(), machine.to_owned());
        match remote.run(machine, setting, command).await {
            Ok(value) => {
                self.kept.lock().unwrap().insert(source, value.clone());
                Ok(value)
            }
            Err(Unanswered::Unreachable(error)) => {
                let kept = self.kept.lock().unwrap().get(&source).cloned();
                kept.ok_or_else(|| {
                    error.context(format!(
                        "{setting} runs on {machine}, which cannot be reached, and this machine keeps no value of it"
                    ))
                })
            }
            Err(Unanswered::Failed(error)) => Err(error.context(format!("{setting} on {machine}"))),
        }
    }

    /// Drop the value kept for `value`, which its consumer refused.
    pub fn forget(&self, value: &Value) {
        if let Value::Cmd {
            cmd,
            on: Some(machine),
        } = value
        {
            let source = (cmd.clone(), machine.clone());
            self.kept.lock().unwrap().remove(&source);
        }
    }

    /// Drop every kept value, as this machine leaves its hub.
    pub fn forget_all(&self) {
        self.kept.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests;
