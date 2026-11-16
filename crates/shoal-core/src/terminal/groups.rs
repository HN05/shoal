//! The hosts and groups a queued start that cannot open its place falls
//! back through: from its origin's group to its host's configured group on
//! the same server, then to the default host's configured group.
use crate::{config::Effective, sessions::Origin, terminal::Configured};

/// The first host a queued start from `origin` tries, unless none can.
pub(crate) fn first_queued_host(
    settings: &Effective,
    origin: Option<&Origin>,
) -> Option<Configured> {
    fallbacks(settings, origin)
        .into_iter()
        .next()
        .map(|fallback| fallback.host)
}

/// A host a queued start may run in, and the group it joins there.
pub struct Fallback<'a> {
    pub host: Configured,
    pub group: Group<'a>,
}

/// The group a queued start joins in its host.
pub enum Group<'a> {
    /// The origin itself.
    Origin(&'a Origin),
    /// The host's configured group, on the server of the origin when given.
    Configured(Option<&'a Origin>),
}

/// Where a queued start from `origin` may run, in the order to try: its own
/// group, its host's configured group on the same server, then the default
/// host's configured group.
pub fn fallbacks<'a>(settings: &Effective, origin: Option<&'a Origin>) -> Vec<Fallback<'a>> {
    let mut fallbacks = Vec::new();
    if let Some(origin) = origin
        && let Some(host) = Configured::all(settings).find(|host| host.takes_queued(origin))
    {
        let keeps_groups = host.options.group_template.is_some();
        fallbacks.push(Fallback {
            host,
            group: Group::Origin(origin),
        });
        if keeps_groups {
            fallbacks.push(Fallback {
                host: Configured::named(settings, origin.host),
                group: Group::Configured(Some(origin)),
            });
        }
    }
    // Config that keeps a host from opening places keeps queued starts out.
    if let Some(host) = Configured::default_host(settings).filter(|host| host.options.opens_places)
    {
        fallbacks.push(Fallback {
            host,
            group: Group::Configured(None),
        });
    }
    fallbacks
}

#[cfg(test)]
mod tests;
