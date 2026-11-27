//! `shoal hub pair`.
use anyhow::Result;

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
    },
    client::output::hub::Paired,
};

pub(super) async fn pair(ctx: &Context) -> Result<i32> {
    let pairing = crate::client::hub::pair(&ctx.paths).await?;
    let url = &pairing.code.url;
    let warnings = (url.unencrypted_remote())
        .then(|| {
            format!(
                "{url} is http:// to another machine, so links cross the network unencrypted; put a proxy or tunnel with TLS in front of the listener and set [machine.hub] url to its https:// address"
            )
        })
        .into_iter()
        .collect();
    let code = pairing.code.to_string();
    let installer = pairing.code.installer();
    let paired = Paired {
        pairing,
        installer,
        warnings,
    };
    ctx.show(&paired, |paired| {
        let palette = Palette::stdout(false);
        outln!("{}", palette.paint(Style::Heading, &code));
        outln!(
            "Run shoal hub join with this code on the machine to join, by {}; it admits one join.",
            paired.pairing.expires_at.local_minutes()
        );
        outln!("On a computer without Shoal, run instead:");
        outln!("  {}", paired.installer);
        for warning in &paired.warnings {
            eprintln!(
                "{} {warning}",
                Palette::stderr(false).paint(Style::Warning, "warning:")
            );
        }
    })?;
    Ok(0)
}
