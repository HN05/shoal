//! What `shoal pause` and `shoal unpause` print.
use crate::output::outputs;

outputs!(
    shoal_protocol::pause::Paused,
    shoal_protocol::pause::Unpaused
);
