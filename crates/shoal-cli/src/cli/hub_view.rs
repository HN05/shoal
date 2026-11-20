//! The hub's view of the other joined machines' spaces, which lists and
//! pickers show beside this machine's.
use shoal_protocol::hub::directory::HubView;

use crate::cli::context::Context;

/// The hub's view of the other machines' spaces the caller sees; none on a
/// machine that takes no part in a hub, and none, with a warning, while the
/// hub cannot be asked.
pub(in crate::cli) async fn view(ctx: &Context) -> Option<HubView> {
    match crate::client::hub::directory(&ctx.paths).await {
        Ok(view) => view,
        Err(error) => {
            eprintln!("warning: the other machines' spaces are left out: {error:#}");
            None
        }
    }
}
