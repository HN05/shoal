//! Wall-clock timestamps, distinct from monotonic durations and deadlines.
use std::time::{SystemTime, UNIX_EPOCH};

/// Whole Unix seconds, or zero when the clock is before the epoch.
pub(crate) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
