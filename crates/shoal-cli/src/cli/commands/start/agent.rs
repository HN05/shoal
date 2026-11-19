//! The model a start's agent runs with, refused before anything else is
//! asked for when its launch cannot pass it on.
use anyhow::Result;

use crate::{
    agent::{Agent, ModelChoice},
    config::Effective,
};

/// Refuse `model` for `agent` before anything else is asked for, when its
/// launch cannot pass the values on.
pub(in crate::cli::commands) fn check_model(
    settings: &Effective,
    agent: &Agent,
    model: &ModelChoice,
) -> Result<()> {
    let resolved = crate::agents::resolve_model(settings, agent, model.clone());
    crate::agents::check_model(settings, agent, &resolved)
}
