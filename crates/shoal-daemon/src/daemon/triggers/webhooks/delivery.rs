//! A request's delivery to its webhook trigger: the route names the
//! trigger, `/webhooks/<name>` for the global config's and
//! `/webhooks/<repository>/<name>` for a repository's; an active trigger
//! takes it once its signature checks out, and fires in the background.
use std::{os::unix::ffi::OsStrExt, sync::Arc};

use anyhow::{Context as _, Result};
use shoal_protocol::triggers::{RunOrigin, WebhookRequest};

use super::super::{Configured, firing::Firing};
use crate::{
    daemon::{log, workspace::Manager},
    time::UnixTime,
    triggers::{
        Source,
        webhook::{self, Signed},
    },
};

/// What a delivery became.
pub(in crate::daemon::triggers) enum Delivered {
    /// Its trigger fires for it.
    Accepted,
    /// No active trigger takes it: none has its route, the trigger is
    /// paused or the signature is wrong, which the caller cannot tell apart.
    Refused,
}

/// What a route names: a repository's trigger, or the global config's.
struct Route<'a> {
    /// The registered repository's name.
    repository: Option<&'a str>,
    name: &'a str,
}

impl<'a> Route<'a> {
    fn parse(path: &'a str) -> Option<Self> {
        let route = path.strip_prefix("/webhooks/")?;
        match route.split('/').collect::<Vec<_>>()[..] {
            [name] => Some(Self {
                repository: None,
                name,
            }),
            [repository, name] => Some(Self {
                repository: Some(repository),
                name,
            }),
            _ => None,
        }
    }
}

impl Manager {
    /// Deliver `request` to the webhook trigger whose route is `path`,
    /// where its signature of `body`, the body as sent, checks out.
    pub(in crate::daemon::triggers) async fn deliver_webhook(
        self: &Arc<Self>,
        path: &str,
        request: WebhookRequest,
        body: &[u8],
    ) -> Result<Delivered> {
        let Some((trigger, signed)) = self.routed_trigger(path).await? else {
            return Ok(Delivered::Refused);
        };
        let at = UnixTime::now();
        if self.trigger_state(&trigger, at).await?.paused {
            return Ok(Delivered::Refused);
        }
        if let Some(signed) = &signed
            && !self
                .signature_verifies(&trigger, signed, &request, body)
                .await?
        {
            return Ok(Delivered::Refused);
        }
        self.fire_webhook(trigger, request, at);
        Ok(Delivered::Accepted)
    }

    /// The webhook trigger whose route is `path`, with how its requests
    /// sign.
    async fn routed_trigger(&self, path: &str) -> Result<Option<(Configured, Option<Signed>)>> {
        let Some(route) = Route::parse(path) else {
            return Ok(None);
        };
        let repository = match route.repository {
            Some(name) => match self.repository_named(name).await? {
                Some(repository) => Some(repository),
                None => return Ok(None),
            },
            None => None,
        };
        let triggers = self.triggers_of(repository.as_ref()).await?;
        let trigger = (triggers.into_iter())
            .find(|trigger| trigger.repository == repository && trigger.name == route.name);
        Ok(trigger.and_then(|trigger| match &trigger.trigger.source {
            Source::Webhook(webhook) => {
                let signed = webhook.signed.clone();
                Some((trigger, signed))
            }
            Source::Timer(_) => None,
        }))
    }

    /// The ID of the one registered repository named `name`.
    async fn repository_named(
        &self,
        name: &str,
    ) -> Result<Option<shoal_ids::repository_ids::RepositoryId>> {
        let repositories = self.repositories().await?;
        let mut named = (repositories.into_iter())
            .filter(|repository| crate::repository::name(repository) == name);
        Ok(match (named.next(), named.next()) {
            (Some(repository), None) => Some(repository.id),
            _ => None,
        })
    }

    /// Whether `request` signs `body` with the secret of `trigger`, whose
    /// value its command fetches now and nothing keeps.
    async fn signature_verifies(
        &self,
        trigger: &Configured,
        signed: &Signed,
        request: &WebhookRequest,
        body: &[u8],
    ) -> Result<bool> {
        let secrets = match &trigger.repository {
            Some(repository) => self.repository_settings(repository).await?.secrets.clone(),
            None => self.config().global_settings()?.secrets,
        };
        let secret = (secrets.get(&signed.secret)).with_context(|| {
            format!(
                "the {} names secret {}, which [secrets] does not configure",
                trigger.label(),
                signed.secret
            )
        })?;
        let value = crate::secrets::fetch(&signed.secret, secret, None).await?;
        Ok(signed.signature.verifies(value.as_bytes(), request, body))
    }

    /// Fire `trigger` for `request`, received at `at`, once the triggers
    /// fire one at a time.
    fn fire_webhook(self: &Arc<Self>, trigger: Configured, request: WebhookRequest, at: UnixTime) {
        let manager = self.clone();
        tokio::spawn(async move {
            let _firing = manager.triggers.firing.lock().await;
            let firing = Firing {
                origin: RunOrigin::Source,
                input: webhook::input(&trigger.name, &request, at),
                at,
            };
            if let Err(error) = manager.fire_trigger(&trigger, firing).await {
                log!("{} did not fire: {error:#}", trigger.label());
            }
        });
    }
}
