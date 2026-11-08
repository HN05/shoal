//! What failed in a cleanup pass, each failure in the space it concerns
//! where it concerns one.
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use std::{collections::BTreeMap, fmt};

/// A step's failures in the spaces they concern. A step returns it as
/// its error so that the pass records each failure for its space rather
/// than for every space.
#[derive(Debug, Default)]
pub(crate) struct SpaceFailures(Vec<SpaceFailure>);

#[derive(Debug)]
struct SpaceFailure {
    id: SpaceId,
    name: String,
    error: String,
}

impl SpaceFailures {
    /// Record that the step failed in the space `id`, named `name`.
    pub(crate) fn push(&mut self, id: &SpaceId, name: &str, error: &anyhow::Error) {
        self.0.push(SpaceFailure {
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

impl fmt::Display for SpaceFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failures: Vec<_> = (self.0.iter())
            .map(|failure| format!("{}: {}", failure.name, failure.error))
            .collect();
        f.write_str(&failures.join("; "))
    }
}

impl std::error::Error for SpaceFailures {}

/// The failed steps of one pass: those that concern no space, and each
/// space's own.
#[derive(Debug, Default)]
pub struct SweepFailures {
    general: Vec<String>,
    /// By space ID.
    spaces: BTreeMap<SpaceId, Failed>,
}

/// A space's failed steps in one pass.
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
        match error.downcast::<SpaceFailures>() {
            Ok(failures) => {
                for failure in failures.0 {
                    let failed = self.spaces.entry(failure.id).or_insert(Failed {
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
        match self.general.is_empty() && self.spaces.is_empty() {
            true => Ok(()),
            false => Err(self),
        }
    }

    /// The failures that concern no space.
    pub(super) fn general(&self) -> Option<String> {
        (!self.general.is_empty()).then(|| self.general.join("; "))
    }

    /// The ID, name and failures of each space with failures.
    pub(super) fn spaces(&self) -> impl Iterator<Item = (&SpaceId, &str, String)> {
        (self.spaces.iter())
            .map(|(id, failed)| (id, failed.name.as_str(), failed.errors.join("; ")))
    }

    /// The failures of the space `id`.
    #[cfg(test)]
    pub(super) fn space(&self, id: &SpaceId) -> Option<String> {
        self.spaces.get(id).map(|failed| failed.errors.join("; "))
    }
}

impl fmt::Display for SweepFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failures: Vec<_> = (self.general.iter().cloned())
            .chain(
                self.spaces()
                    .map(|(_, name, errors)| format!("{name}: {errors}")),
            )
            .collect();
        f.write_str(&failures.join("; "))
    }
}

/// The failures of the pass before, which repeat on every pass until fixed,
/// so that the log has each once.
#[derive(Debug, Default)]
pub(super) struct LoggedFailures(Option<String>);

impl LoggedFailures {
    /// The failures of a pass with `result` to log: none when they are
    /// those of the pass before.
    pub(super) fn new_failures(&mut self, result: &Result<(), SweepFailures>) -> Option<String> {
        let failed = result.as_ref().err().map(ToString::to_string);
        let repeated = failed == self.0;
        self.0.clone_from(&failed);
        failed.filter(|_| !repeated)
    }
}

#[cfg(test)]
mod tests;
