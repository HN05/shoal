//! The tests of coordination across subsystems, which drive a whole
//! daemon: the worktrees changes and code references follow, chats' marked
//! turns, holds and leases approvals decide, the removal, restore and
//! rename of spaces, request dispatch, attached files and the hub.
mod alerts;
mod alerts_sending;
mod approvals;
mod changes;
mod permissions;
mod permissions_actions;
mod scope_access;
mod threads;
mod threads_code;
