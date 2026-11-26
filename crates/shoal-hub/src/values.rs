//! Values a machine's settings take from a command another machine of the
//! hub runs. Only a machine's daemon asks, never a device or a scoped
//! caller.
use std::sync::{Arc, Weak};

use anyhow::{Result, anyhow, ensure};
use futures_util::future::BoxFuture;
use shoal_core::config::values::{Remote, Unanswered};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    config::value::Resolved,
    hub::{
        Member, MemberKind,
        link::{Call, Reply},
    },
    time::UnixTime,
};

use crate::{Hub, daemon::Daemon, log, members, value_fetches};

/// The daemon's way to the other machines of its hub.
struct ThroughHub {
    hub: Arc<Hub>,
    daemon: Weak<dyn Daemon>,
}

impl Remote for ThroughHub {
    fn run<'a>(
        &'a self,
        machine: &'a str,
        setting: &'a str,
        command: &'a [String],
    ) -> BoxFuture<'a, Result<String, Unanswered>> {
        Box::pin(async move {
            let daemon = (self.daemon.upgrade())
                .ok_or_else(|| Unanswered::Unreachable(anyhow!("the daemon is stopping")))?;
            let asked = Asked {
                machine,
                setting,
                command,
            };
            self.hub.fetch_value(&*daemon, asked).await
        })
    }
}

/// A setting's request for what its command prints on another machine.
pub(super) struct Asked<'a> {
    pub(super) machine: &'a str,
    pub(super) setting: &'a str,
    pub(super) command: &'a [String],
}

/// The machine that asks for a value.
pub(super) struct Requester<'a> {
    pub(super) id: &'a MemberId,
    pub(super) name: &'a str,
}

impl Hub {
    /// Have the daemon reach other machines' values through this hub.
    pub(crate) fn connect_values(self: &Arc<Self>, daemon: &Arc<dyn Daemon>) {
        let remote = ThroughHub {
            hub: self.clone(),
            daemon: Arc::downgrade(daemon),
        };
        self.config.values().connect(Arc::new(remote));
    }

    /// What `asked` prints on the machine it names: run here for this
    /// machine, else routed by the hub, which is this daemon or the one its
    /// link reaches.
    async fn fetch_value(
        &self,
        daemon: &dyn Daemon,
        asked: Asked<'_>,
    ) -> Result<String, Unanswered> {
        let (id, name) = (self.own_identity(daemon).await).map_err(Unanswered::Unreachable)?;
        if id.as_str() == asked.machine || name.as_str() == asked.machine {
            return run_value(asked.command).await.map_err(Unanswered::Failed);
        }
        let reply = if self.is_hub(daemon) {
            let own = Requester {
                id: &id,
                name: &name,
            };
            (self.route_value(own, asked).await).map_err(Unanswered::Failed)?
        } else {
            self.ask_hub(asked).await?
        };
        answered(reply)
    }

    /// The hub's reply to this machine's request `asked`, over its link:
    /// unreachable without one, or once it fails.
    async fn ask_hub(&self, asked: Asked<'_>) -> Result<Reply, Unanswered> {
        let calls = (self.hub_calls().await).map_err(Unanswered::Unreachable)?;
        let call = Call::Value {
            machine: asked.machine.to_owned(),
            setting: asked.setting.to_owned(),
            command: asked.command.to_vec(),
        };
        (calls.call(call).await).map_err(unanswered)
    }

    /// The ID and name of this machine as its hub knows it: the hub's own,
    /// or the one it joined as.
    async fn own_identity(&self, daemon: &dyn Daemon) -> Result<(MemberId, String)> {
        if self.is_hub(daemon) {
            let own = self.own_member().await?;
            return Ok((own, crate::process::machine::hostname()));
        }
        let joined = self.store.read(crate::membership::load).await?;
        let joined = joined.ok_or_else(|| anyhow!("this machine is joined to no hub"))?;
        Ok((joined.membership.member, joined.membership.name))
    }

    /// The hub's answer to `from`'s request `asked`: what the machine it
    /// names printed, which the hub records it fetched, never the value,
    /// or that the machine is offline.
    pub(super) async fn route_value(&self, from: Requester<'_>, asked: Asked<'_>) -> Result<Reply> {
        let target = self.value_machine(asked.machine).await?;
        let reply = match target.hub {
            true => Reply::Value(Resolved(run_value(asked.command).await?)),
            false => self.ask_value_machine(&target, from.name, &asked).await,
        };
        if let Reply::Value(_) = reply {
            self.record_fetch(from.id, &target.id, asked.setting)
                .await?;
        }
        Ok(reply)
    }

    /// The reply of `target`, another machine than the hub's, to the
    /// request `asked` of the machine named `requester`: unreachable while
    /// its link is down, or once it ends meanwhile.
    async fn ask_value_machine(
        &self,
        target: &Member,
        requester: &str,
        asked: &Asked<'_>,
    ) -> Reply {
        let Some(calls) = self.links.calls(&target.id) else {
            let message = super::routing::offline(target).to_string();
            return Reply::Unreachable { message };
        };
        let call = Call::RunValue {
            requester: requester.to_owned(),
            setting: asked.setting.to_owned(),
            command: asked.command.to_vec(),
        };
        let error = match calls.call(call).await {
            Ok(reply) => return reply,
            Err(error) => error.context(target.name.clone()),
        };
        match unanswered(error) {
            Unanswered::Unreachable(error) => Reply::Unreachable {
                message: format!("{error:#}"),
            },
            Unanswered::Failed(error) => Reply::Failed {
                message: format!("{error:#}"),
            },
        }
    }

    /// Record that `requester` fetched `setting` from `machine` now.
    async fn record_fetch(
        &self,
        requester: &MemberId,
        machine: &MemberId,
        setting: &str,
    ) -> Result<()> {
        let fetch = value_fetches::Fetch {
            requester: requester.clone(),
            machine: machine.clone(),
            setting: setting.to_owned(),
            at: UnixTime::now(),
        };
        (self.store)
            .write(move |tx| value_fetches::record(tx, &fetch))
            .await
    }

    /// The machine `named` names by ID or name, which runs a value's
    /// command.
    async fn value_machine(&self, named: &str) -> Result<Member> {
        let own = self.hub_identity().await?.public.clone();
        let selector = named.to_owned();
        let member = (self.store)
            .read(move |tx| members::select(tx, &selector, &own))
            .await?;
        ensure!(
            member.kind == MemberKind::Machine,
            "{named} is a device, which runs no command"
        );
        Ok(member)
    }

    /// This machine's answer to the hub's call to run `command` for the
    /// setting `setting` of the machine named `requester`.
    pub(super) async fn serve_value(
        &self,
        requester: &str,
        setting: &str,
        command: &[String],
    ) -> Result<Reply> {
        log!("hub: running the command of {setting} for {requester}");
        Ok(Reply::Value(Resolved(run_value(command).await?)))
    }
}

/// Why a call over a link gave no reply: unreachable when the link failed,
/// before or after the call went, and failed when the other side answered
/// with a failure, such as a command that failed there.
fn unanswered(error: anyhow::Error) -> Unanswered {
    let link_failed =
        error.is::<crate::session::Unsent>() || error.is::<crate::session::Unanswered>();
    match link_failed {
        true => Unanswered::Unreachable(error),
        false => Unanswered::Failed(error),
    }
}

/// The value `reply` carries, or why it carries none.
fn answered(reply: Reply) -> Result<String, Unanswered> {
    match reply {
        Reply::Value(Resolved(value)) => Ok(value),
        Reply::Unreachable { message } => Err(Unanswered::Unreachable(anyhow!(message))),
        Reply::Failed { message } => Err(Unanswered::Failed(anyhow!(message))),
        reply => Err(Unanswered::Failed(super::answers::unexpected(reply))),
    }
}

/// What `command` prints on this machine, as the daemon runs its own
/// values' commands.
async fn run_value(command: &[String]) -> Result<String> {
    shoal_core::config::value::bounded_output(command).await
}

#[cfg(test)]
mod tests;
