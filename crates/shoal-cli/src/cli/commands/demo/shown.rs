//! `shoal internal show-demo`: the demo's machine's side of opening a
//! terminal demo from another machine, run by the daemon on a terminal it
//! made for that user here.
use anyhow::Result;

use crate::{sessions::Origin, terminal::Elsewhere};

/// Show the demo `name`, which runs in `place`, on this terminal, which a
/// user on another machine views, as the place's host shows a place there;
/// `machine` names this one, and `app` is the label that user's app for
/// the host reports, if any.
pub(in crate::cli::commands) async fn show_for_viewer(
    name: &str,
    place: &Origin,
    machine: &str,
    app: Option<&str>,
) -> Result<i32> {
    let viewer = Elsewhere { machine, app };
    crate::terminal::show_place_elsewhere(place, &format!("demo {name}"), &viewer).await?;
    Ok(0)
}
