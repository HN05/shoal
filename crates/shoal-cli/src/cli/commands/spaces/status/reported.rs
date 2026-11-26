//! The status of a space whose machine is offline, as that machine last
//! reported it.
use anyhow::Result;
use shoal_protocol::{status::ReportedStatus, time::UnixTime};

use crate::cli::{
    commands::progress,
    context::Context,
    list_status::ListStatusStyle,
    output::{Palette, Style},
};

pub(super) fn show(ctx: &Context, reported: &ReportedStatus) -> Result<()> {
    let palette = Palette::stdout(ctx.json);
    ctx.show(reported, |reported| render(reported, palette))
}

fn render(reported: &ReportedStatus, palette: Palette) {
    let space = &reported.space.space;
    outln!(
        "{}  {}",
        palette.paint(Style::Heading, &space.name),
        palette.paint(space.status.style(), space.status.label())
    );
    outln!("Machine:       {}", reported.machine.name);
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
    let note = format!(
        "{}; this is what it last reported",
        reported.machine.offline(UnixTime::now())
    );
    outln!("{}", palette.paint(Style::Muted, &note));
}
