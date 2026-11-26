//! The repositories the other joined machines register.
use anyhow::{Context as _, Result};
use shoal_client::output::repositories::RepositoryElsewhere;
use shoal_protocol::hub::directory::HubView;

use crate::cli::context::Context;

/// The repositories the other joined machines register, with the hub's
/// view of those machines; none on a machine in no hub.
pub(super) async fn registered(
    ctx: &Context,
) -> Result<(Vec<RepositoryElsewhere>, Option<HubView>)> {
    let view = (crate::client::repositories::elsewhere(&ctx.paths).await)
        .context("cannot ask the hub for the other machines' repositories")?;
    let listed = view.as_ref().map(of).unwrap_or_default();
    Ok((listed, view))
}

/// [`registered`], or none, with a warning, while the hub cannot list
/// them.
pub(super) async fn registered_or_left_out(
    ctx: &Context,
) -> (Vec<RepositoryElsewhere>, Option<HubView>) {
    registered(ctx).await.unwrap_or_else(|error| {
        eprintln!("warning: the other machines' repositories are left out: {error:#}");
        (Vec::new(), None)
    })
}

fn of(view: &HubView) -> Vec<RepositoryElsewhere> {
    let directory = &view.directory;
    let mut listed: Vec<RepositoryElsewhere> = (directory.repositories.iter())
        .filter_map(|listed| {
            let machine = directory.machine(&listed.machine)?;
            Some(RepositoryElsewhere {
                machine: machine.name.clone(),
                machine_id: machine.id.to_string(),
                registered: listed.repository.clone(),
            })
        })
        .collect();
    listed.sort_by(|a, b| (&a.machine, a.name()).cmp(&(&b.machine, b.name())));
    listed
}
