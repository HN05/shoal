//! What a failed command prints with `--json`, on standard error.
use serde::{Deserialize, Serialize};

use crate::output::outputs;

/// A failed command's JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandError {
    pub error: ErrorDetail,
}

/// Why a command failed. Act on `code`, never on the message, which may
/// change: the protocol's code of a refusal, such as `not_found` or
/// `setup_failed`, or `command_failed` for a failure without one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

outputs!(CommandError);
