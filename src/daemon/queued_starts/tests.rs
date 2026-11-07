/// Each start with the environment its worker would receive.
pub(crate) static STARTED: std::sync::Mutex<
    Vec<(String, std::collections::BTreeMap<String, String>)>,
> = std::sync::Mutex::new(Vec::new());
