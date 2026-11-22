//! The app registered as the placer, which an `external` policy asks: an
//! unscoped client of the hub's machine registers over a connection it
//! keeps open, the latest registration taking over from the one before,
//! and answers each snapshot the hub sends it by the number it came with.
//! The hub waits [`WAIT`] for an answer, so a start never waits long on an
//! app.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use shoal_protocol::hub::placement::{Asked, Unanswered};
use shoal_schedule::placement::{Placement, snapshot::Snapshot};
use tokio::sync::{mpsc, oneshot};

/// How long the hub waits for the placer's answer.
pub(crate) const WAIT: Duration = Duration::from_secs(2);

/// The registered placer, if any, and the answers the hub waits for.
#[derive(Default)]
pub(crate) struct Placers(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    /// The registered placer's number and where its snapshots go.
    serving: Option<(u64, mpsc::UnboundedSender<Asked>)>,
    /// The number the next registration or snapshot takes.
    next: u64,
    /// Where each snapshot's answer goes, by its number.
    waiting: HashMap<u64, oneshot::Sender<Placement>>,
}

/// A registration as the placer, which ends when it drops, unless a later
/// one took over.
pub struct Registration {
    state: Arc<Mutex<State>>,
    number: u64,
    /// The snapshots the hub asks it to place.
    pub asked: mpsc::UnboundedReceiver<Asked>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        if state
            .serving
            .as_ref()
            .is_some_and(|(number, _)| *number == self.number)
        {
            state.serving = None;
            // What it was asked goes unanswered at once.
            state.waiting.clear();
        }
    }
}

impl Placers {
    /// Register a placer in place of the one before, whose snapshots stop.
    pub fn register(&self) -> Registration {
        let (sender, asked) = mpsc::unbounded_channel();
        let mut state = self.0.lock().unwrap();
        let number = state.take_number();
        state.serving = Some((number, sender));
        Registration {
            state: self.0.clone(),
            number,
            asked,
        }
    }

    /// The registered placer's placement of `snapshot`, or why it gave
    /// none within `wait`.
    pub async fn ask(&self, snapshot: &Snapshot, wait: Duration) -> Result<Placement, Unanswered> {
        let (number, answer) = self.send(snapshot)?;
        let answered = tokio::time::timeout(wait, answer).await;
        self.0.lock().unwrap().waiting.remove(&number);
        match answered {
            Ok(Ok(placement)) => Ok(placement),
            // Its registration ended meanwhile.
            Ok(Err(_)) => Err(Unanswered::NoPlacer),
            Err(_) => Err(Unanswered::Late),
        }
    }

    /// Take `placement` as the answer to the snapshot `number`; false once
    /// the hub no longer waits for it.
    pub fn answer(&self, number: u64, placement: Placement) -> bool {
        let waiting = self.0.lock().unwrap().waiting.remove(&number);
        waiting.is_some_and(|answer| answer.send(placement).is_ok())
    }

    /// Send `snapshot` to the registered placer, with the number its
    /// answer comes by and where it arrives.
    fn send(&self, snapshot: &Snapshot) -> Result<(u64, oneshot::Receiver<Placement>), Unanswered> {
        let mut state = self.0.lock().unwrap();
        let number = state.take_number();
        let (answer, answered) = oneshot::channel();
        let asked = Asked {
            number,
            snapshot: snapshot.clone(),
        };
        let sent = (state.serving.as_ref()).is_some_and(|(_, serving)| serving.send(asked).is_ok());
        if !sent {
            return Err(Unanswered::NoPlacer);
        }
        state.waiting.insert(number, answer);
        Ok((number, answered))
    }
}

impl State {
    fn take_number(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
}

#[cfg(test)]
mod tests;
