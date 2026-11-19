//! The agent an addition starts: named, configured or picked.

use anyhow::Result;

use crate::{
    agent::{Agent, ModelChoice},
    cli::{agents, context::Context},
    protocol::ConfigTarget,
    start_plan::ResolvedAddAgent,
};

/// The effective settings of the repository a workspace is added in, or of
/// the existing workspace a launch goes to, loaded on first use.
pub(super) struct AddSettings<'a> {
    ctx: &'a Context,
    target: ConfigTarget,
    effective: tokio::sync::OnceCell<crate::config::Effective>,
}

impl<'a> AddSettings<'a> {
    pub(super) fn new(ctx: &'a Context, repository: &crate::repository::Selector) -> Self {
        Self::of(ctx, ConfigTarget::Repository(repository.into()))
    }

    /// The settings of the existing workspace `id`.
    pub(super) fn workspace(ctx: &'a Context, id: &shoal_ids::space_ids::SpaceId) -> Self {
        Self::of(ctx, ConfigTarget::Space(id.into()))
    }

    fn of(ctx: &'a Context, target: ConfigTarget) -> Self {
        Self {
            ctx,
            target,
            effective: tokio::sync::OnceCell::new(),
        }
    }

    pub(super) async fn get(&self) -> Result<&crate::config::Effective> {
        self.effective
            .get_or_try_init(|| shoal_workers::settings::of(&self.ctx.paths, self.target.clone()))
            .await
    }
}

/// Refuse `model` for `agent` when its launch cannot pass it on.
fn check_model(
    settings: &crate::config::Effective,
    agent: &Agent,
    model: &ModelChoice,
) -> Result<()> {
    let resolved = crate::agents::resolve_model(settings, agent, model.clone());
    crate::agents::check_model(settings, agent, &resolved)
}

/// The agent a start runs, as [`agents::start_agent`] chooses it from the
/// one `named`, refusing a `model` it cannot run with before anything is
/// created; one it leaves to the daemon takes the model there.
pub(super) async fn resolve_add_agent(
    ctx: &Context,
    settings: &AddSettings<'_>,
    named: Option<Agent>,
    model: ModelChoice,
) -> Result<ResolvedAddAgent> {
    let settings = settings.get().await?;
    let agent = agents::start_agent(ctx, settings, named)?;
    if let Some(agent) = &agent {
        check_model(settings, agent, &model)?;
    }
    Ok(ResolvedAddAgent { agent, model })
}
