//! The rows of other joined machines' spaces, as their machines last
//! reported them, in the tree of every machine's spaces or a list of their
//! own.
use shoal_protocol::hub::directory::Listed as Remote;

use super::{
    Cell, Machines, WidthLimit, aligned_to, indented, number_cell, progress_cell, space_mode_cell,
    status_cell, status_line, title_limit,
};
use crate::cli::{
    list_status::ListStatusStyle,
    output::{Palette, Style},
};

/// One row per space of `remote`, each naming its machine after its name,
/// cut to fit `width` as the tree's rows are.
pub fn remote_rows(
    remote: &[&Remote],
    machines: &Machines<'_>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let remote: Vec<_> = remote.iter().map(|remote| (*remote, None)).collect();
    judged_remote_rows(&remote, machines, palette, width)
}

/// One row per space of `remote` as [`remote_rows`] gives it, with the
/// reason a command would refuse it in place of its status where it has
/// one.
pub fn judged_remote_rows(
    remote: &[(&Remote, Option<&str>)],
    machines: &Machines<'_>,
    palette: Palette,
    width: Option<usize>,
) -> Vec<String> {
    let (markers, cells): (Vec<_>, Vec<_>) = (remote.iter())
        .map(|(remote, refusal)| {
            let repository = (remote.space.repository.as_ref())
                .map(|origin| origin.name.clone())
                .unwrap_or_default();
            let (marker, mut cells) = remote_row(remote, (repository, None), 0, palette);
            if let Some(refusal) = refusal {
                cells[STATUS] = ((*refusal).to_owned(), Some(Style::Muted));
            }
            (marker, with_machine(cells, machines.cell(&remote.machine)))
        })
        .unzip();
    (markers.into_iter())
        .zip(aligned_to(cells, palette, machine_title_limit(width)))
        .map(|(marker, row)| format!("{marker} {row}"))
        .collect()
}

/// The width limit of rows whose machine column comes before the title.
pub(super) fn machine_title_limit(width: Option<usize>) -> Option<WidthLimit> {
    title_limit(width).map(|limit| WidthLimit {
        column: limit.column + 1,
        ..limit
    })
}

/// The column of [`remote_row`]'s cells that holds the space's status.
const STATUS: usize = 4;

/// The row of another machine's space, as its machine last reported it,
/// naming the agent it runs where its status line does not take the place.
pub(super) fn remote_row(
    remote: &Remote,
    repository: Cell,
    depth: usize,
    palette: Palette,
) -> (String, [Cell; 8]) {
    let space = &remote.space;
    (
        space.status.marker(palette),
        [
            number_cell(space.number),
            (indented(&space.name, depth), Some(Style::Heading)),
            repository,
            space_mode_cell(space.kind, space.mode.as_ref()),
            status_cell(space.status),
            (String::new(), None),
            progress_cell(space.progress.as_ref()),
            (
                status_line(space.progress.as_ref())
                    .or(space.running.as_ref().map(|agent| agent.name()))
                    .unwrap_or_default()
                    .to_owned(),
                None,
            ),
        ],
    )
}

/// `cells` with `machine` after the space's name.
pub(super) fn with_machine([number, name, rest @ ..]: [Cell; 8], machine: Cell) -> [Cell; 9] {
    let [a, b, c, d, e, f] = rest;
    [number, name, machine, a, b, c, d, e, f]
}
