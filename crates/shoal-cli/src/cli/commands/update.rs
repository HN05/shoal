//! `shoal update`.
use anyhow::Result;

use crate::{
    cli::context::Context,
    client::output::update::Update,
    update::{Installed, RELEASES, Release, Target, platform},
};

pub(super) async fn run(ctx: &Context, version: Option<String>) -> Result<i32> {
    let installation = Installed::current()?.downloaded()?;
    let target = installation.path().to_owned();
    let current = env!("CARGO_PKG_VERSION");
    let wanted = version.map_or(Target::Latest, Target::Exact);
    let suffix = installation.asset_suffix(&platform()?);
    let release = ctx
        .progress(
            "Checking for the release",
            Release::find(RELEASES, &wanted, &suffix),
        )
        .await?;
    if !wanted.replaces(release.version(), current)? {
        ctx.emit(
            &format!("Shoal {current} is up to date"),
            Update {
                updated: false,
                previous: None,
                version: current.to_owned(),
                latest: Some(release.version().to_owned()),
                path: target,
            },
        )?;
        return Ok(0);
    }
    ctx.progress(
        &format!("Installing Shoal {}", release.version()),
        release.install(&installation),
    )
    .await?;
    ctx.emit(
        &format!(
            "Updated Shoal {current} to {} at {}\nA managed daemon restarts with it once idle; run `shoal daemon restart` to apply it now.",
            release.version(),
            target.display()
        ),
        Update {
            updated: true,
            previous: Some(current.to_owned()),
            version: release.version().to_owned(),
            latest: None,
            path: target,
        },
    )?;
    Ok(0)
}
