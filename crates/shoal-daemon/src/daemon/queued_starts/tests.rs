/// Each start with the environment its worker would receive.
pub(crate) static STARTED: std::sync::Mutex<
    Vec<(
        shoal_ids::queue_entry_ids::QueueEntryId,
        std::collections::BTreeMap<String, String>,
    )>,
> = std::sync::Mutex::new(Vec::new());
