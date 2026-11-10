//! `shoal log`: a space's log, oldest first, a line for each entry
//! with its local time, a label and its details, under a heading for each
//! date; `--json` prints each entry as one JSON line.
use anyhow::Result;

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::{
        log::{LogEntry, LogFilter, LogTopic},
        space_names::SpaceSelector,
        subscriptions::{Subscription, Update, records::Record},
    },
};

mod entries;

pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    all: bool,
    limit: u32,
    follow: bool,
) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let filter = if all {
        LogFilter::All
    } else {
        LogFilter::Entries
    };
    let mut printer = Printer::new(ctx);
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

/// Print the log's newest entries, then each new one, until interrupted. A
/// snapshot after a daemon restart prints only what is new.
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

/// Prints entries in order, with a date heading where the date changes.
struct Printer<'a> {
    ctx: &'a Context,
    palette: Palette,
    date: Option<String>,
    /// The newest entry printed.
    last: Option<crate::client::events::EventCursor>,
}

impl<'a> Printer<'a> {
    fn new(ctx: &'a Context) -> Self {
        Self {
            ctx,
            palette: Palette::stdout(ctx.json),
            date: None,
            last: None,
        }
    }

    /// Print `entry` when it is newer than every entry printed.
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
        outln!("{}", line(time, entry, self.palette));
        Ok(())
    }
}

/// The line of `entry` at `time`: its label padded to the longest label, so
/// lines printed apart still align, and its details.
fn line(time: &str, entry: &LogEntry, palette: Palette) -> String {
    let (label, details) = entries::describe(entry);
    let label = format!("{label:<width$}", width = entries::LABEL_WIDTH);
    let line = format!("{time}  {label}  {details}");
    let line = line.trim_end();
    // Every event shows those the log leaves out dimmed.
    match entry.is_entry() {
        true => line.to_owned(),
        false => palette.paint(Style::Muted, line),
    }
}
