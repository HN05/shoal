//! Looking forge blockers up as the user, each through the forge crate's
//! clearance of its kind.
use std::collections::HashSet;

use super::{Lookup, Lookups, Target};
use crate::{
    daemon::workspace::Manager,
    forge::{
        clearance::{Clearance, NewestRuns, clearance},
        items::link::LinkKind,
    },
    state::BlockerKind,
};

impl Manager {
    /// What the forge says of each of the forge blockers among `targets`.
    pub(super) async fn look_up_blockers(&self, targets: HashSet<Target>) -> Lookups {
        let account = self.user_account();
        let hosts = self.forge_hosts();
        let mut newest = NewestRuns::default();
        let mut lookups = Lookups::new();
        for (kind, target) in targets {
            let link = match kind {
                BlockerKind::Issue => LinkKind::Issue,
                BlockerKind::Pr => LinkKind::Pr,
                BlockerKind::CiRun => LinkKind::Ci,
                BlockerKind::Space | BlockerKind::Launch => continue,
            };
            let lookup = match clearance(link, &target, &hosts, &account, &mut newest).await {
                Ok(Clearance::Cleared(how)) => Lookup::Cleared(how),
                Ok(Clearance::Open) => Lookup::Open,
                Err(error) => Lookup::Failed(format!("{error:#}")),
            };
            lookups.insert((kind, target), lookup);
        }
        lookups
    }
}
