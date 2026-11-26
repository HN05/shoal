use super::*;
use crate::process::load::MemoryPressure;
use crate::stand_in::Fixture;

const CALM: MemoryPressure = MemoryPressure {
    critical: false,
    stalled: false,
    warning: false,
    safe: true,
};

/// Past the stop level of memory in use.
const CRITICAL: MemoryPressure = MemoryPressure {
    critical: true,
    warning: true,
    safe: false,
    ..CALM
};

const PROTECTIONS_OFF: &str =
    "[machine.overload.memory]\nenabled = false\n[machine.overload.disk]\nenabled = false\n";

/// A sampler with its handoff open, sampling at `at` and then as each
/// sample asks.
struct Samples {
    sampler: Sampler,
    _handoff: Handoff,
    at: Instant,
}

impl Samples {
    fn new(fixture: &Fixture) -> Self {
        let (sampler, handoff) = Sampler::new(fixture.agents.clone(), fixture.daemon.clone());
        Self {
            sampler,
            _handoff: handoff,
            at: Instant::now(),
        }
    }

    fn sample(&mut self) {
        let wait = self.sampler.sample(self.at).expect("the handoff is open");
        self.at += wait;
    }

    fn during(&mut self, duration: Duration) {
        let end = self.at + duration;
        while self.at < end {
            self.sample();
        }
    }

    /// Sample until `done` holds, returning when the sample that made it
    /// hold was taken.
    fn until(&mut self, done: impl Fn() -> bool) -> Instant {
        // A hang guard, well past any recovery window.
        let end = self.at + Duration::from_secs(3600);
        while self.at < end {
            let at = self.at;
            self.sample();
            if done() {
                return at;
            }
        }
        panic!("never held within an hour of samples");
    }
}

#[tokio::test]
async fn recovery_progresses_after_every_protection_is_disabled() {
    let fixture = Fixture::new().await;
    fixture.configure(PROTECTIONS_OFF);
    let recovery = fixture.agents.overload_recovery();
    Samples::new(&fixture).until(|| recovery.published().is_some());
}

#[tokio::test]
async fn recovery_waits_for_free_disk_space() {
    let fixture = Fixture::new().await;
    let mut config = crate::config::Config::default();
    config.machine.overload.memory.enabled = false;
    let window = Duration::from_secs(config.machine.overload.recovery.sustained_seconds);
    fixture.agents.config.publish(config);
    let recovery = fixture.agents.overload_recovery();
    let mut samples = Samples::new(&fixture);
    samples.during(Duration::from_secs(600));
    assert_eq!(recovery.published(), None);
    let generation = fixture.agents.config.generation();
    fixture.agents.config.publish_disk_reading(Some(generation));
    let recovered = samples.at;
    let published = samples.until(|| recovery.published().is_some());
    assert!(published - recovered >= window);
}

#[tokio::test]
async fn unsafe_readings_revoke_the_recovery_epoch() {
    let fixture = Fixture::new().await;
    let mut config = crate::config::Config::default();
    config.machine.overload.memory.enabled = false;
    fixture.agents.config.publish(config);
    let generation = fixture.agents.config.generation();
    fixture.agents.config.publish_disk_reading(Some(generation));
    let recovery = fixture.agents.overload_recovery();
    let mut samples = Samples::new(&fixture);
    samples.until(|| recovery.published().is_some());
    // Admitting a restore advances the epoch it then holds while it checks
    // its workspace.
    recovery.reset();
    let admitted = recovery.epoch();
    fixture.agents.config.publish_disk_reading(None);
    samples.until(|| recovery.epoch() != admitted);
    assert_eq!(*recovery.watch().borrow(), None);
}

/// Publish a calm config, then `change` it once most of the recovery
/// window has passed: the window starts over.
async fn a_reload_restarts_the_recovery_window(change: fn(&mut crate::config::Config)) {
    let fixture = Fixture::new().await;
    let calm = || {
        let mut config = crate::config::Config::default();
        config.machine.overload.memory.enabled = false;
        config.machine.overload.disk.enabled = false;
        config
    };
    let mut changed = calm();
    change(&mut changed);
    let window = Duration::from_secs(changed.machine.overload.recovery.sustained_seconds);
    fixture.agents.config.publish(calm());
    let recovery = fixture.agents.overload_recovery();
    let mut samples = Samples::new(&fixture);
    let old_window = Duration::from_secs(calm().machine.overload.recovery.sustained_seconds);
    samples.during(old_window * 2 / 3);
    assert_eq!(recovery.published(), None);
    fixture.agents.config.publish(changed);
    let reloaded = samples.at;
    let published = samples.until(|| recovery.published().is_some());
    assert!(published - reloaded >= window);
}

#[tokio::test]
async fn a_changed_threshold_restarts_the_recovery_window() {
    a_reload_restarts_the_recovery_window(|config| {
        config.machine.overload.recovery.sustained_seconds += 1;
    })
    .await;
}

#[tokio::test]
async fn a_changed_memory_limit_restarts_the_recovery_window() {
    a_reload_restarts_the_recovery_window(|config| {
        config.machine.memory_limit = crate::config::memory_limit::MemoryLimitSetting::Limit(
            crate::config::memory_limit::MemoryLimit::Bytes(8 << 30),
        );
    })
    .await;
}

#[tokio::test]
async fn a_stop_is_handed_over_one_at_a_time_without_waiting_on_it() {
    let fixture = Fixture::new().await;
    let config = crate::config::Config::default();
    let poll = Duration::from_secs(config.machine.overload.poll_seconds);
    let cooldown = Duration::from_secs(config.machine.overload.cooldown_seconds);
    fixture.agents.config.publish(config);
    let (mut sampler, mut handoff) = Sampler::new(fixture.agents.clone(), fixture.daemon.clone());
    let critical = || Readings {
        memory: Some(MemoryReading {
            memory: load::Memory {
                total: 100,
                available: 1,
            },
            stall: None,
            work: None,
            pressure: CRITICAL,
        }),
        limited: false,
        cpu: Ok(load::CpuTicks::default()),
        disk_safe: true,
    };
    let start = Instant::now();
    sampler.decide(start, critical());
    assert_eq!(handoff.stops.try_recv(), Ok(Pressure::Memory));
    sampler.decide(start + poll, critical());
    assert!(handoff.stops.try_recv().is_err());
    // A stop that stopped something waits out the cooldown before the next
    // sample, after which another stop may follow.
    handoff.outcomes.send(true).unwrap();
    assert_eq!(sampler.sample(start + poll * 2), Some(cooldown));
    sampler.decide(start + poll * 2 + cooldown, critical());
    assert_eq!(handoff.stops.try_recv(), Ok(Pressure::Memory));
    assert!(handoff.warnings.try_recv().is_ok());
}

/// Time that passes as fast as the thread samples.
struct Virtual(Instant);

impl Clock for Virtual {
    fn now(&self) -> Instant {
        self.0
    }

    fn sleep(&mut self, duration: Duration) {
        self.0 += duration;
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn samples_keep_coming_while_the_runtime_is_blocked() {
    let fixture = Fixture::new().await;
    fixture.configure(PROTECTIONS_OFF);
    let (sampler, handoff) = Sampler::new(fixture.agents.clone(), fixture.daemon.clone());
    let thread = std::thread::spawn(move || sampler.run(Virtual(Instant::now())));
    // This blocks the test's runtime, whose only thread it is, while the
    // sampler publishes recovery, which takes a window of samples.
    let guard = std::time::Instant::now() + Duration::from_secs(600);
    while fixture.agents.overload_recovery().published().is_none() {
        assert!(
            std::time::Instant::now() < guard,
            "recovery was never published"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(handoff);
    thread.join().unwrap();
}
