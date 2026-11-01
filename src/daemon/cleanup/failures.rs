//! What failed in a cleanup pass, each failure in the workspace it concerns
//! where it concerns one.
use anyhow::Result;
use std::{collections::BTreeMap, fmt};

/// A step's failures in the workspaces they concern. A step returns it as
/// its error so that the pass records each failure for its workspace rather
/// than for every workspace.
#[derive(Debug, Default)]
pub(crate) struct WorkspaceFailures(Vec<WorkspaceFailure>);

#[derive(Debug)]
struct WorkspaceFailure {
    id: String,
    name: String,
    error: String,
}

impl WorkspaceFailures {
    /// Record that the step failed in the workspace `id`, named `name`.
    pub(crate) fn push(&mut self, id: &str, name: &str, error: &anyhow::Error) {
        self.0.push(WorkspaceFailure {
            id: id.to_owned(),
            name: name.to_owned(),
            error: format!("{error:#}"),
        });
    }

    /// Fails with the recorded failures, if there are any.
    pub(crate) fn into_result(self) -> Result<()> {
        match self.0.is_empty() {
            true => Ok(()),
            false => Err(self.into()),
        }
    }
}

impl fmt::Display for WorkspaceFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failures: Vec<_> = (self.0.iter())
            .map(|failure| format!("{}: {}", failure.name, failure.error))
            .collect();
        f.write_str(&failures.join("; "))
    }
}

impl std::error::Error for WorkspaceFailures {}

/// The failed steps of one pass: those that concern no workspace, and each
/// workspace's own.
#[derive(Debug, Default)]
pub struct SweepFailures {
    general: Vec<String>,
    /// By workspace ID.
    workspaces: BTreeMap<String, Failed>,
}

/// A workspace's failed steps in one pass.
#[derive(Debug)]
struct Failed {
    name: String,
    errors: Vec<String>,
}

impl SweepFailures {
    /// Record the result of the step `step`.
    pub(super) fn step(&mut self, step: &str, result: Result<()>) {
        let Err(error) = result else {
            return;
        };
        match error.downcast::<WorkspaceFailures>() {
            Ok(failures) => {
                for failure in failures.0 {
                    let failed = self.workspaces.entry(failure.id).or_insert(Failed {
                        name: failure.name,
                        errors: Vec::new(),
                    });
                    failed.errors.push(format!("{step}: {}", failure.error));
                }
            }
            Err(error) => self.general.push(format!("{step}: {error:#}")),
        }
    }

    /// Fails with the recorded failures, if there are any.
    pub(super) fn into_result(self) -> Result<(), Self> {
        match self.general.is_empty() && self.workspaces.is_empty() {
            true => Ok(()),
            false => Err(self),
        }
    }

    /// The failures that concern no workspace.
    pub(super) fn general(&self) -> Option<String> {
        (!self.general.is_empty()).then(|| self.general.join("; "))
    }

    /// The ID, name and failures of each workspace with failures.
    pub(super) fn workspaces(&self) -> impl Iterator<Item = (&str, &str, String)> {
        (self.workspaces.iter())
            .map(|(id, failed)| (id.as_str(), failed.name.as_str(), failed.errors.join("; ")))
    }

    /// The failures of the workspace `id`.
    #[cfg(test)]
    pub(super) fn workspace(&self, id: &str) -> Option<String> {
        self.workspaces
            .get(id)
            .map(|failed| failed.errors.join("; "))
    }
}

impl fmt::Display for SweepFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failures: Vec<_> = (self.general.iter().cloned())
            .chain(
                self.workspaces()
                    .map(|(_, name, errors)| format!("{name}: {errors}")),
            )
            .collect();
        f.write_str(&failures.join("; "))
    }
}

#[cfg(test)]
mod tests;
