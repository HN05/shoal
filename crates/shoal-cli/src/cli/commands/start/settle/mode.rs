//! The mode a start's agent starts in: named, else on a terminal the one
//! its issue's labels call for or a picked one, else the default mode.
use anyhow::Result;
use shoal_protocol::launches::{IssueChoice, Launch};

use crate::{
    cli::{
        args::create::StartArgs,
        commands::{issues, start::DaemonReads},
        context::Context,
        ui::{self, ModeChoice, ModeFit},
    },
    config::{Effective, modes},
    launches,
    model::{Repository, SpaceKind},
};

/// The mode of `launch` as far as nothing needs asking: the one it names,
/// off a terminal the [`launches::default_mode`], on one the plan mode for
/// an issue to pick, which no other mode takes, or the mode the labels of
/// the issue it names in the `found` repository call for under
/// `settings`; otherwise one a terminal picks once the daemon has checked
/// the rest.
pub(super) async fn decided(
    ctx: &Context,
    launch: &Launch,
    found: Option<&Repository>,
    settings: &Effective,
) -> Result<ModeChoice> {
    if let Some(mode) = &launch.mode {
        return Ok(ModeChoice::Named(mode.clone()));
    }
    if !ui::can_pick(ctx) {
        return Ok(ModeChoice::Named(launches::default_mode(launch)));
    }
    if launch.issue == Some(IssueChoice::Pick) {
        return Ok(ModeChoice::Named(modes::ModeName::PLAN));
    }
    let labelled = match found {
        Some(repo) => issue_mode(ctx, launch, repo, settings).await?,
        None => None,
    };
    Ok(labelled.map_or(ModeChoice::Pick, ModeChoice::Named))
}

/// Refuse the copy and fork pickers that making the launch of `args`
/// opens where the mode they name starts a kind of space that takes
/// neither, before either picker asks anything.
pub(super) async fn ensure_located_fit(ctx: &Context, args: &StartArgs) -> Result<()> {
    let Some(mode) = args
        .mode
        .as_ref()
        .filter(|_| args.copy.is_set() || args.fork)
    else {
        return Ok(());
    };
    let launch = Launch {
        directory: ui::current_directory()?,
        ..args.launch()
    };
    let settings = launches::shape::launch_settings(&DaemonReads(ctx), &launch).await?;
    let kind = modes::get(&settings.modes, mode)?.kind;
    super::arguments::ensure_flags_fit(args, kind, mode)
}

/// The mode `choice` names, else one picked under `settings` among those
/// that start the kind of space the arguments of `launch` other than the
/// mode fit, in a repository where they name one. A setup's mode is built
/// in, which no config defines.
pub(super) fn chosen(
    ctx: &Context,
    choice: ModeChoice,
    launch: &Launch,
    settings: &Effective,
) -> Result<modes::ModeName> {
    if let (ModeChoice::Named(mode), Some(_)) = (&choice, launch.setup) {
        return Ok(mode.clone());
    }
    let names_repository = launch.in_workspace.is_some() || launches::names_repository(launch)?;
    let fit = ModeFit {
        kind: launches::fitting_kind(launch),
        repository: names_repository.then_some(true),
    };
    choice.chosen(ctx, &settings.modes, fit)
}

/// The mode `mode` of `launch` under `settings`, which says what a
/// terminal picks for it; none for a setup's, which its setup settles.
pub(super) fn configured<'a>(
    launch: &Launch,
    mode: &modes::ModeName,
    settings: &'a Effective,
) -> Result<Option<&'a modes::Mode>> {
    match launch.setup {
        Some(_) => Ok(None),
        None => modes::get(&settings.modes, mode).map(Some),
    }
}

/// Whether `mode` starts the agent of `launch` in a workspace under
/// `settings`, where a terminal picks what the workspace starts from.
pub(super) fn starts_workspace(
    launch: &Launch,
    mode: &modes::ModeName,
    settings: &Effective,
) -> Result<bool> {
    let configured = configured(launch, mode, settings)?;
    Ok(configured.is_some_and(|mode| mode.kind == SpaceKind::Workspace))
}

/// The mode the labels of the issue `launch` names in `repo` call for
/// under `settings`, by [`launches::labelled_mode`]; none without such an
/// issue or a label to look for, or for arguments that fit one kind of
/// space, whose picker the labels do not answer.
async fn issue_mode(
    ctx: &Context,
    launch: &Launch,
    repo: &Repository,
    settings: &Effective,
) -> Result<Option<modes::ModeName>> {
    if launches::fitting_kind(launch).is_some() {
        return Ok(None);
    }
    let Some(input) = launches::named_issue(launch)? else {
        return Ok(None);
    };
    if settings.issue.plan_label.is_none() && settings.issue.planned_label.is_none() {
        return Ok(None);
    }
    let context = issues::read_context(ctx, repo).await?;
    let issue = issues::load(ctx, context, input).await?;
    let mode = launches::labelled_mode(&settings.issue, &issue.labels);
    Ok(mode.filter(|mode| modes::get(&settings.modes, mode).is_ok()))
}
