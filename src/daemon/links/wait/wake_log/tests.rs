use super::{Ending, Updates, ending};
use crate::daemon::links::wait::UpdatesExt;

#[test]
fn only_a_wait_that_ends_without_updates_is_logged() {
    let delivered = Updates::messages(Vec::new());
    assert_eq!(ending(&Ok(delivered)), None);
    assert_eq!(ending(&Ok(Updates::superseded())), Some(Ending::Superseded));
    let failed = ending(&Err(anyhow::anyhow!("scope_denied")));
    assert_eq!(failed, Some(Ending::Failed("scope_denied".into())));
}
