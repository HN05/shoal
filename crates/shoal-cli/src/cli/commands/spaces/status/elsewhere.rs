//! The status of another joined machine's space, as its machine last
//! reported it to the hub; the rest of its status stays on that machine.
use anyhow::{Result, bail};
use shoal_protocol::{
    hub::directory::{HubView, Listed},
    space_names::SpaceSelector,
};

use crate::cli::{
    commands::{machines, progress},
    context::Context,
    list_status::ListStatusStyle,
    output::{Palette, Style},
    ui,
};

/// The other machine's space `selector` names when no space of this
/// machine has it: by its ID, or by a number only one other machine's
/// space has, with the hub's view that names its machine.
pub(super) async fn find(
    ctx: &Context,
    selector: &SpaceSelector,
) -> Result<Option<(Listed, HubView)>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    if spaces.find(selector).is_some() {
        return Ok(None);
    }
    let Some(view) = machines::view(ctx).await else {
        return Ok(None);
    };
    let mut found = (view.directory.spaces.iter())
        .filter(|remote| selector.selects(&remote.space.id, remote.space.number));
    match (found.next().cloned(), found.next()) {
        (Some(remote), None) => Ok(Some((remote, view))),
        (Some(_), Some(_)) => bail!(
            "several other machines have a space {selector}; name it by the ID shoal list --json shows"
        ),
        (None, _) => Ok(None),
    }
}

/// Show `remote`, whose machine `view` names.
pub(super) fn render(remote: &Listed, view: &HubView, palette: Palette) {
    let space = &remote.space;
    outln!(
        "{}  {}",
        palette.paint(Style::Heading, &space.name),
        palette.paint(space.status.style(), space.status.label())
    );
    let (machine, _) = ui::Machines::new(view).cell(&remote.machine);
    outln!("Machine:       {machine}");
    outln!("Number:        {}", space.number);
    outln!("Kind:          {}", space.kind);
    if let Some(repository) = &space.repository {
        outln!("Repository:    {}", repository.name);
    }
    if let Some(mode) = &space.mode {
        outln!("Mode:          {mode}");
    }
    if let Some(parent) = &space.parent {
        outln!("Parent:        {parent}");
    }
    progress::render_section(space.progress.as_ref(), palette);
    for question in &space.questions {
        outln!("Question {}:  {}", question.number, question.one_line());
    }
    for request in &space.approvals {
        let about = crate::cli::commands::approvals::about(request);
        outln!("Approval {}:  {about}", request.number);
    }
    let name = (view.directory.machine(&remote.machine)).map_or_else(
        || remote.machine.to_string(),
        |machine| machine.name.clone(),
    );
    let note = format!("shoal status on {name} shows the rest of its status");
    outln!("{}", palette.paint(Style::Muted, &note));
}
