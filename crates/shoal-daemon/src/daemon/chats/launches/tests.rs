use std::sync::Mutex;

use crate::protocol::ChatStart;

/// The resolved starts the daemon ran its chat start worker for, each with
/// whether the worker acts with the caller's scope.
pub(crate) static STARTED: Mutex<Vec<(ChatStart, bool)>> = Mutex::new(Vec::new());
