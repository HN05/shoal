//! Where this machine ranks among the hub's machines for a waiting CI job,
//! which delays its claim; `docs/design/runner.md#placement-delays-a-claim`
//! says why.
use anyhow::{Result, anyhow};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    hub::link::{Call, Reply},
    model::Repository,
    repository::url_key,
};
use shoal_schedule::placement::{
    Balanced, Placer, Policy,
    snapshot::{Lifetime, Request, Snapshot},
};

use super::placed_machines;
use crate::{Hub, daemon::Daemon};

impl Hub {
    /// Where this machine ranks, 0 first, for a CI job of `repository`: 0
    /// in no hub, under `local`, which keeps work where it is found, and
    /// for a repository without an origin, which matches no other
    /// machine's. It fails while the hub cannot be asked.
    pub async fn rank_job(&self, daemon: &dyn Daemon, repository: &Repository) -> Result<usize> {
        let url = crate::repository::remote_url_from_source(&repository.source).await?;
        let Some(origin) = url.as_deref().map(url_key) else {
            return Ok(0);
        };
        if self.is_hub(daemon) {
            let own = self.own_member().await?;
            return self.rank_job_for(daemon, job_request(origin, own)).await;
        }
        if self.this_machine(daemon).await?.is_none() {
            return Ok(0);
        }
        let request = job_request(origin, MemberId::from(String::new()));
        match self.through_hub(Call::RankJob { request }).await? {
            Reply::Ranked(rank) => Ok(rank),
            reply => Err(anyhow!("the hub answered with an unexpected {reply:?}")),
        }
    }

    /// As the hub: where the caller of `request` ranks among the connected
    /// machines, by `balanced`'s stages under any policy but `local`. An
    /// `external` placer places starts, which the user waits on, not jobs.
    pub(crate) async fn rank_job_for(
        &self,
        daemon: &dyn Daemon,
        request: Request,
    ) -> Result<usize> {
        if self.config.current().machine.hub.placement == Policy::Local {
            return Ok(0);
        }
        let members = self.local_hub_overview(daemon).await?.members;
        let caller = request.caller.clone();
        let machines = placed_machines(members.iter(), request.origin.as_deref());
        let snapshot = Snapshot { request, machines };
        Ok(Balanced.place(&snapshot).rank_of(&caller))
    }
}

/// A CI job of the repository of `origin`, waiting on the machine
/// `caller`: disposable, since nothing keeps it once it ends, and heavy, as
/// the agent queue never counts a job light. It starts no agent tool.
fn job_request(origin: String, caller: MemberId) -> Request {
    Request {
        origin: Some(origin),
        base: None,
        agent: None,
        heavy: true,
        lifetime: Lifetime::Disposable,
        caller,
        parent: None,
    }
}
