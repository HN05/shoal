//! The CI jobs a machine's sleep or shutdown cut off, which the hub keeps
//! and reruns in its place.
use std::time::Duration;

use anyhow::{Result, anyhow};
use rusqlite::{Transaction, params};
use shoal_daemon_base::alerts::About;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        Member, MemberKind,
        link::{Call, Reply},
        reruns::{CutOffJob, CutOffRerun},
    },
};

use crate::{Hub, answers::unexpected, daemon::Daemon, log, membership, time::UnixTime};

/// How often the hub tries the reruns of the jobs it keeps.
const RERUN_EVERY: Duration = Duration::from_secs(60);

/// How long after its stop a job waits for its run to allow its rerun.
const PATIENCE: Duration = Duration::from_secs(24 * 60 * 60);

/// A job the hub keeps to rerun, which the machine `member` cut off.
#[derive(Debug)]
struct Held {
    job: CutOffJob,
    member: MemberId,
}

/// What becomes of a job the hub keeps, once it tried its rerun.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The forge reruns it.
    Reran,
    /// It needs no rerun.
    Settled,
    /// It waits for the next try.
    Waiting,
    /// It is given up, for the reason given.
    GivenUp(String),
}

impl Hub {
    /// As a machine joined to a hub: hand the hub `jobs`, which this
    /// machine cut off and recorded to rerun, returning whether the hub
    /// keeps them now; false on the hub's own machine and on one in no
    /// hub, which rerun their jobs themselves.
    pub async fn hand_cut_off_jobs(
        &self,
        daemon: &dyn Daemon,
        jobs: Vec<CutOffJob>,
    ) -> Result<bool> {
        if jobs.is_empty() || self.is_hub(daemon) {
            return Ok(false);
        }
        if self.store.read(membership::load).await?.is_none() {
            return Ok(false);
        }
        let call = Call::CutOff {
            jobs,
            sent_at: UnixTime::now(),
        };
        match self.through_hub(call).await? {
            Reply::Delivered => Ok(true),
            reply => Err(unexpected(reply)),
        }
    }

    /// As the hub: keep `jobs`, which `member` cut off, to rerun them in
    /// its place, and try their reruns now.
    pub(crate) async fn hear_cut_off(&self, member: &MemberId, jobs: Vec<CutOffJob>) -> Result<()> {
        let member = member.clone();
        (self.store)
            .write(move |tx| keep(tx, &member, &jobs))
            .await?;
        self.cut_off_jobs.notify_one();
        Ok(())
    }

    /// Try the reruns of the jobs the hub keeps every [`RERUN_EVERY`] and
    /// whenever a machine hands some over, while this daemon is the hub,
    /// until it stops.
    pub(crate) async fn keep_rerunning_cut_off_jobs(&self, daemon: &dyn Daemon) {
        let mut every = tokio::time::interval(RERUN_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = every.tick() => {}
                () = self.cut_off_jobs.notified() => {}
            }
            if !self.is_hub(daemon) {
                continue;
            }
            if let Err(error) = self.rerun_cut_off_jobs(daemon, UnixTime::now()).await {
                log!("hub: could not rerun the CI jobs machines cut off: {error:#}");
            }
        }
    }

    /// Try the rerun of each job the hub keeps, forgetting those rerun,
    /// settled or given up by `now`, and tell the user of those it reran.
    async fn rerun_cut_off_jobs(&self, daemon: &dyn Daemon, now: UnixTime) -> Result<()> {
        let held = self.store.read(held_jobs).await?;
        if held.is_empty() {
            return Ok(());
        }
        let machines = self.rerunning_machines(daemon).await?;
        let mut reran = Vec::new();
        for held in held {
            let rerun = self.rerun_anywhere(daemon, &machines, &held.job).await;
            match outcome(&held.job, rerun, now) {
                Outcome::Waiting => continue,
                Outcome::Reran => reran.push(self.reran_line(daemon, &held).await),
                Outcome::Settled => {}
                Outcome::GivenUp(reason) => self.alert_given_up(daemon, &held, &reason).await,
            }
            let job = held.job;
            (self.store).write(move |tx| forget(tx, &job)).await?;
        }
        self.notify_reran(daemon, &reran).await;
        Ok(())
    }

    /// The connected machines but the hub's own, which a job's rerun goes
    /// to when the hub's own machine has no login for its forge.
    async fn rerunning_machines(&self, daemon: &dyn Daemon) -> Result<Vec<Member>> {
        let members = self.local_hub_overview(daemon).await?.members;
        Ok((members.into_iter())
            .filter(|member| member.kind == MemberKind::Machine && member.connected && !member.hub)
            .collect())
    }

    /// Rerun `job` as the user on the hub's own machine, else on the first
    /// of `machines` that answers with the user's login for its forge.
    async fn rerun_anywhere(
        &self,
        daemon: &dyn Daemon,
        machines: &[Member],
        job: &CutOffJob,
    ) -> Result<CutOffRerun> {
        let mut rerun = daemon.rerun_cut_off(job).await;
        for machine in machines {
            if answered(&rerun) {
                break;
            }
            rerun = latest(rerun, self.rerun_on(&machine.id, job).await);
        }
        rerun
    }

    /// Have the connected machine `id` rerun `job` as the user.
    async fn rerun_on(&self, id: &MemberId, job: &CutOffJob) -> Result<CutOffRerun> {
        let calls = (self.links.calls(id)).ok_or_else(|| anyhow!("machine {id} is offline"))?;
        match calls.call(Call::RerunCutOff { job: job.clone() }).await? {
            Reply::CutOffRerun(rerun) => Ok(rerun),
            reply => Err(unexpected(reply)),
        }
    }

    /// How the user's notice names `held`, which the hub reran.
    async fn reran_line(&self, daemon: &dyn Daemon, held: &Held) -> String {
        let machine = self.member_name(daemon, &held.member).await;
        let job = &held.job;
        log!(
            "hub: reran {} of run {}, which machine {machine} cut off",
            job.name,
            job.run
        );
        format!(
            "{} of {}, cut off on machine {machine} by {}",
            job.name, job.run, job.cut_off_by
        )
    }

    async fn notify_reran(&self, daemon: &dyn Daemon, reran: &[String]) {
        if reran.is_empty() {
            return;
        }
        let message = format!(
            "Reran CI jobs on their forges, where any runner can take them: {}",
            reran.join("; ")
        );
        (self.notify(daemon, About::default(), AlertKind::JobsRerun, message)).await;
    }

    /// Tell the user that `held` is not rerun, for `reason`.
    async fn alert_given_up(&self, daemon: &dyn Daemon, held: &Held, reason: &str) {
        let machine = self.member_name(daemon, &held.member).await;
        let job = &held.job;
        let message = format!(
            "CI job {}, which machine {machine} cut off, was not rerun: {reason}; rerun {} on its forge",
            job.name, job.run
        );
        (self.notify(daemon, About::default(), AlertKind::RunnerFailed, message)).await;
    }
}

/// Whether a machine's try of a rerun answered it: one without the user's
/// login for the forge, or that failed, leaves it to the next machine.
fn answered(rerun: &Result<CutOffRerun>) -> bool {
    matches!(rerun, Ok(rerun) if *rerun != CutOffRerun::NoLogin)
}

/// What the tries of a rerun found, `earlier` and then `later`, which
/// keeps an earlier failure over a later machine without a login, so a
/// job given up names why.
fn latest(earlier: Result<CutOffRerun>, later: Result<CutOffRerun>) -> Result<CutOffRerun> {
    match (earlier, later) {
        (Err(error), Ok(CutOffRerun::NoLogin)) => Err(error),
        (Err(error), later) => {
            log!("hub: a rerun of a CI job a machine cut off failed: {error:#}");
            later
        }
        (Ok(_), later) => later,
    }
}

/// What becomes of `job`, given what its `rerun` found by `now`: given up
/// once it has waited [`PATIENCE`] since its stop.
fn outcome(job: &CutOffJob, rerun: Result<CutOffRerun>, now: UnixTime) -> Outcome {
    let expired = now.duration_since(job.stopped_at) >= PATIENCE;
    let reason = match rerun {
        Ok(CutOffRerun::Rerun) => return Outcome::Reran,
        Ok(CutOffRerun::Settled) => return Outcome::Settled,
        _ if !expired => {
            if let Err(error) = &rerun {
                log!("hub: rerun of {} of run {}: {error:#}", job.name, job.run);
            }
            return Outcome::Waiting;
        }
        Ok(CutOffRerun::Unfinished) => "its run has not finished".to_owned(),
        Ok(CutOffRerun::NoLogin) => {
            "no connected machine has the user's login for its forge".to_owned()
        }
        Err(error) => format!("{error:#}"),
    };
    Outcome::GivenUp(reason)
}

/// Keep `jobs`, which `member` cut off, beside those the hub keeps.
fn keep(tx: &Transaction<'_>, member: &MemberId, jobs: &[CutOffJob]) -> Result<()> {
    let mut insert = tx.prepare(
        "INSERT OR REPLACE INTO hub_ci_job_reruns(run_url,job_id,member_id,job_name,cut_off_by,stopped_at)
         VALUES (?1,?2,?3,?4,?5,?6)",
    )?;
    for job in jobs {
        insert.execute(params![
            job.run,
            job.job,
            member,
            job.name,
            job.cut_off_by,
            job.stopped_at
        ])?;
    }
    Ok(())
}

fn held_jobs(tx: &Transaction<'_>) -> Result<Vec<Held>> {
    let mut statement = tx.prepare(
        "SELECT run_url,job_id,job_name,cut_off_by,stopped_at,member_id FROM hub_ci_job_reruns",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(Held {
            job: CutOffJob {
                run: row.get(0)?,
                job: row.get(1)?,
                name: row.get(2)?,
                cut_off_by: row.get(3)?,
                stopped_at: row.get(4)?,
            },
            member: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Forget `job`, which needs no more tries.
fn forget(tx: &Transaction<'_>, job: &CutOffJob) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_ci_job_reruns WHERE run_url=?1 AND job_id=?2",
        params![job.run, job.job],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
