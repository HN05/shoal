//! The status of another joined machine's space: its machine's, which the
//! daemon asks it for through the hub, without the sessions and transcripts
//! that stay there, or, while that machine is offline, what it last
//! reported to the hub.
use anyhow::Result;
use shoal_protocol::{
    hub::directory::{HubView, Listed},
    space_names::SpaceSelector,
};

use super::{StatusView, render::render_status};

use crate::cli::{
    commands::progress,
    context::Context,
    list_status::ListStatusStyle,
    output::{Palette, Style},
    ui,
};

/// Show the status of `remote`, whose machine `view` names: that
/// machine's while it is connected, else what it last reported.
pub(super) async fn show(ctx: &Context, remote: &Listed, view: &HubView) -> Result<()> {
    let connected =
        (view.directory.machine(&remote.machine)).is_some_and(|machine| machine.connected);
    if !connected {
        let palette = Palette::stdout(ctx.json);
        return ctx.show(remote, |remote| render(remote, view, palette));
    }
    let selector = SpaceSelector::id(&remote.space.id);
    let status = crate::client::workspaces::status(&ctx.paths, selector).await?;
    let status = StatusView {
        status,
        sessions: Vec::new(),
        sessions_error: None,
        transcripts: Vec::new(),
        transcripts_error: None,
        machine: Some(remote.machine.clone()),
    };
    ctx.show(&status, |status| {
        render_status(status, ctx.json);
        let (machine, _) = ui::Machines::new(view).cell(&remote.machine);
        let note = format!("Runs on machine {machine}, which keeps its sessions and transcripts");
        outln!("{}", Palette::stdout(ctx.json).paint(Style::Muted, &note));
    })
}

/// Show `remote`, whose machine `view` names, as it last reported it.
fn render(remote: &Listed, view: &HubView, palette: Palette) {
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
    for thread in &space.threads {
        outln!("Thread {}:  {}", thread.number, thread.one_line());
    }
    for request in &space.approvals {
        let about = crate::cli::commands::approvals::about(request);
        outln!("Approval {}:  {about}", request.number);
    }
    let name = (view.directory.machine(&remote.machine)).map_or_else(
        || remote.machine.to_string(),
        |machine| machine.name.clone(),
    );
    let note = format!("Machine {name} is offline; this is what it last reported");
    outln!("{}", palette.paint(Style::Muted, &note));
}
