//! The inbox's items of the other joined machines' spaces, as their machines
//! last reported them.
use shoal_protocol::{
    hub::directory::{HubView, Listed},
    list_status::ListStatus,
};

use crate::{
    cli::{list_status::ListStatusStyle, output::Style, ui::Cell},
    client::output::inbox::{Item, Need},
};

/// The statuses of another machine's space that need the user.
const NEEDS_USER: [ListStatus; 5] = [
    ListStatus::NeedsInput,
    ListStatus::Blocked,
    ListStatus::Failed,
    ListStatus::Asleep,
    ListStatus::Ready,
];

/// The threads that wait for the user, then the approval requests the user
/// decides, then
/// the statuses that need the user, of the other machines' spaces in `view`
/// that `keeps` keeps.
pub(super) fn items(view: Option<&HubView>, keeps: impl Fn(&Listed) -> bool) -> Vec<Item> {
    let spaces: Vec<&Listed> = (view.into_iter())
        .flat_map(|view| &view.directory.spaces)
        .filter(|remote| keeps(remote))
        .collect();
    // Each item names its need, so it carries its space without the
    // space's other threads and requests.
    let bare: Vec<Listed> = spaces
        .iter()
        .map(|remote| remote.without_requests())
        .collect();
    let item = |space: &Listed, need| Item::Elsewhere {
        space: space.clone(),
        need,
    };
    let each = || spaces.iter().zip(&bare);
    let threads = each().flat_map(|(remote, bare)| {
        (remote.space.threads.iter())
            .filter(|thread| thread.waits_for_user())
            .map(|thread| item(bare, Need::Thread(Box::new(thread.clone()))))
    });
    let approvals = each().flat_map(|(remote, bare)| {
        (remote.space.approvals.iter())
            .filter(|request| super::user_decides(request))
            .map(|request| item(bare, Need::Approval(request.clone())))
    });
    let statuses = each()
        .filter(|(remote, _)| NEEDS_USER.contains(&remote.space.status))
        .map(|(remote, bare)| item(bare, Need::Status(remote.space.status)));
    threads.chain(approvals).chain(statuses).collect()
}

/// What kind of item `need` is, with a thread's or request's number.
pub(super) fn kind(need: &Need) -> Cell {
    match need {
        Need::Thread(thread) => (format!("thread {}", thread.number), Some(Style::Warning)),
        Need::Approval(request) => (format!("approval {}", request.number), Some(Style::Warning)),
        Need::Status(status) => (status.label().into(), Some(status.style())),
    }
}

/// What the row of `remote`'s `need` says: the thread, the access
/// requested and why, or its agent's status line.
pub(super) fn text(remote: &Listed, need: &Need) -> String {
    match need {
        Need::Thread(thread) => thread.one_line(),
        Need::Approval(request) => super::super::approvals::about(request),
        Need::Status(_) => (remote.space.progress.as_ref())
            .and_then(|progress| progress.status.clone())
            .unwrap_or_default(),
    }
}
