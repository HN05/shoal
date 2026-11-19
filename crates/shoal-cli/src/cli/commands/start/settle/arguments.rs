//! What a launch's arguments alone refuse, before anything is looked up or
//! a picker opens.
use anyhow::{Result, ensure};
use shoal_protocol::launches::{Launch, StartTarget};

use crate::{cli::args::create::StartArgs, config::permissions::PermissionChange};

/// Refuse what `launch` asks for that its arguments alone refuse, as
/// [`check_start_arguments`] and [`crate::launches`] refuse it, a mode only
/// its own commands start included, taking its positional argument as the
/// start decides it.
pub(super) fn check(launch: &mut Launch) -> Result<()> {
    check_start_arguments(launch)?;
    crate::launches::ensure_in_fits(launch)?;
    if let Some(mode) = &launch.mode {
        crate::launches::ensure_mode_fits(launch, mode)?;
    }
    // A number beside a branch or issue of the start's own names a
    // repository rather than an item.
    launch.target = crate::launches::decided_target(launch)?;
    Ok(())
}

/// Refuse what `launch` asks for that its arguments alone refuse: two
/// repositories, a link beside a branch, an issue or a copy, and a branch
/// that is not a literal Git branch name.
pub(in crate::cli::commands) fn check_start_arguments(launch: &Launch) -> Result<()> {
    let positional = crate::launches::decided_target(launch)?;
    ensure!(
        launch.repository.is_none() || !matches!(positional, Some(StartTarget::Repository(_))),
        "a positional repository cannot be combined with --repo"
    );
    let links = !matches!(positional, None | Some(StartTarget::Repository(_)));
    ensure!(
        !links || (launch.branch.is_none() && launch.issue.is_none()),
        "an item or branch link cannot be combined with --branch or --issue"
    );
    crate::launches::ensure_copy_fits(launch)?;
    if let Some(branch) = &launch.branch {
        shoal_core::git::check_branch_name(branch)?;
    }
    Ok(())
}

/// Refuse a `--permission KEY=mode`, which drops a value given before, and
/// a new agent has none.
pub(super) fn ensure_nothing_dropped(args: &StartArgs) -> Result<()> {
    ensure!(
        PermissionChange::combined(&args.permissions)
            .dropped
            .is_empty(),
        "--permission KEY={} drops a value given before, and a new agent has none; leave the key out",
        crate::config::permissions::MODE_VALUE
    );
    Ok(())
}
