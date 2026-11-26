//! `shoal log`.
use anyhow::Result;

use crate::{
    cli::{
        args::space::LogArgs,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::subscriptions::Subscription,
};
use shoal_protocol::{
    log::{LogEntry, LogFilter, LogTopic},
    subscriptions::{Update, records::Record},
};

mod entries;

pub(super) async fn run(ctx: &Context, args: LogArgs) -> Result<i32> {
    let LogArgs {
        space,
        all,
        limit,
        follow,
    } = args;
    let space = match space {
        Some(space) => {
            Some(ui::select_space(ctx, Some(space), Fallback::CurrentDirectoryOnly).await?)
        }
        None => ui::space_here(ctx, ui::Workspaces::Below).await?,
    };
    let filter = if all {
        LogFilter::All
    } else {
        LogFilter::Entries
    };
    let mut printer = Printer::new(ctx, space.is_none());
    if follow {
        let topic = LogTopic {
            space,
            filter,
            limit,
        };
        return follow_log(ctx, topic, &mut printer).await;
    }
    let entries = crate::client::log::log(&ctx.paths, space, filter, limit).await?;
    for entry in &entries {
        printer.print(entry)?;
    }
    if entries.is_empty() && !ctx.json {
        outln!("No entries");
    }
    Ok(0)
}

/// A snapshot after a daemon restart prints only what is new.
async fn follow_log(ctx: &Context, topic: LogTopic, printer: &mut Printer<'_>) -> Result<i32> {
    let mut subscription = Subscription::follow_log(ctx.paths.clone(), topic).await?;
    loop {
        let records = match subscription.next().await? {
            Update::Snapshot { records, .. } => records,
            Update::Changed(record) => vec![*record],
            Update::Removed(_) => continue,
        };
        for record in records {
            if let Record::LogEntry(entry) = record {
                printer.print_new(&entry)?;
            }
        }
    }
}

/// The width space names are padded to in Shoal's log, which a longer name
/// exceeds.
const SPACE_WIDTH: usize = 24;

struct Printer<'a> {
    ctx: &'a Context,
    /// Whether lines name their space, as in Shoal's log.
    spaces: bool,
    palette: Palette,
    date: Option<String>,
    last: Option<shoal_protocol::events::EventCursor>,
}

impl<'a> Printer<'a> {
    fn new(ctx: &'a Context, spaces: bool) -> Self {
        Self {
            ctx,
            spaces,
            palette: Palette::stdout(ctx.json),
            date: None,
            last: None,
        }
    }

    fn print_new(&mut self, entry: &LogEntry) -> Result<()> {
        if self.last.is_some_and(|last| entry.id() <= last) {
            return Ok(());
        }
        self.print(entry)
    }

    fn print(&mut self, entry: &LogEntry) -> Result<()> {
        self.last = Some(entry.id());
        if self.ctx.json {
            outln!("{}", crate::client::output::json(entry)?);
            return Ok(());
        }
        let stamp = entry.created_at().local_minutes();
        let (date, time) = stamp.split_once(' ').unwrap_or(("", &stamp));
        if self.date.as_deref() != Some(date) {
            if self.date.is_some() {
                outln!();
            }
            outln!("{}", self.palette.paint(Style::Heading, date));
            self.date = Some(date.to_owned());
        }
        let space = self.spaces.then(|| entries::space(entry));
        outln!("{}", line(time, space.as_deref(), entry, self.palette));
        Ok(())
    }
}

/// Space and label are padded to a fixed width, so lines printed apart still
/// align.
fn line(time: &str, space: Option<&str>, entry: &LogEntry, palette: Palette) -> String {
    let (label, details) = entries::describe(entry);
    let label = format!("{label:<width$}", width = entries::LABEL_WIDTH);
    let line = match space {
        Some(space) => format!(
            "{time}  {}  {label}  {details}",
            shoal_text::pad(space, SPACE_WIDTH)
        ),
        None => format!("{time}  {label}  {details}"),
    };
    let line = line.trim_end();
    // Every event shows those the log leaves out dimmed.
    match entry.is_entry() {
        true => line.to_owned(),
        false => palette.paint(Style::Muted, line),
    }
}
