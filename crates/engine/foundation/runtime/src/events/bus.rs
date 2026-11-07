use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use tokio::sync::Notify;

use super::{Event, TransferKind};
use crate::{OperationId, StepId};

/// Fan-out channel for [`Event`]s; cloning gives another handle to the same channel.
///
/// Publishing never blocks, and every subscriber has its own queue of up
/// to `capacity` events, so a slow reader never slows coblet or the other
/// readers down. Progress is kept fresh rather than complete: a new
/// [`Transfer`](Event::Transfer) or [`StepProgress`](Event::StepProgress)
/// replaces the queued one of the same batch or step, moving to the end of
/// the queue, so a reader that falls behind sees the latest totals instead
/// of a backlog. When the queue is full anyway, the oldest game log copy is
/// dropped first, then the oldest progress, and only then the oldest of the
/// other events; the reader learns how many were dropped. Replaced progress
/// is not counted as dropped.
///
/// # Examples
///
/// ```
/// use coblet_runtime::{Event, EventBus, Received, Warning, WarningCode};
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let bus = EventBus::new(16);
/// let mut events = bus.subscribe();
/// bus.publish(Event::Warning(Warning {
///     op: None,
///     code: WarningCode::from_static("catalog.offline-cache"),
///     message: "using cached version list".to_owned(),
/// }));
/// assert!(matches!(events.recv().await, Some(Received::Event(Event::Warning(_)))));
/// # });
/// ```
#[derive(Clone)]
pub struct EventBus {
    shared: Arc<Shared>,
}

/// Receiving end of an [`EventBus`].
pub struct EventSubscription {
    slot: Arc<Slot>,
}

/// Item delivered to a subscriber.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Received {
    /// The next event.
    Event(Event),
    /// The subscriber fell behind and this many events were dropped.
    Missed(u64),
}

/// State shared by the handles of one bus; dropping the last handle ends
/// every subscription once its queue is read.
struct Shared {
    capacity: usize,
    slots: Mutex<Vec<Weak<Slot>>>,
}

/// Queue of one subscriber.
#[derive(Default)]
struct Slot {
    state: Mutex<Queue>,
    ready: Notify,
}

#[derive(Default)]
struct Queue {
    events: VecDeque<Event>,
    missed: u64,
    closed: bool,
}

/// What a progress event is about; a newer event with the same key
/// replaces the queued one.
#[derive(PartialEq, Eq)]
enum ProgressKey {
    Transfer(OperationId, TransferKind),
    Step(OperationId, StepId),
}

fn progress_key(event: &Event) -> Option<ProgressKey> {
    match event {
        Event::Transfer { op, kind, .. } => Some(ProgressKey::Transfer(*op, *kind)),
        Event::StepProgress { op, step, .. } => Some(ProgressKey::Step(*op, *step)),
        _ => None,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Slot {
    fn push(&self, event: Event, capacity: usize) {
        let mut queue = lock(&self.state);
        if let Some(key) = progress_key(&event) {
            let queued = queue
                .events
                .iter()
                .position(|known| progress_key(known).as_ref() == Some(&key));
            if let Some(index) = queued {
                queue.events.remove(index);
            }
        }
        if queue.events.len() >= capacity {
            let events = &queue.events;
            let victim = events
                .iter()
                .position(|known| matches!(known, Event::GameLog { .. }))
                .or_else(|| {
                    events
                        .iter()
                        .position(|known| progress_key(known).is_some())
                })
                .unwrap_or(0);
            queue.events.remove(victim);
            queue.missed += 1;
        }
        queue.events.push_back(event);
        drop(queue);
        self.ready.notify_one();
    }

    fn close(&self) {
        lock(&self.state).closed = true;
        self.ready.notify_one();
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        for slot in lock(&self.slots).iter().filter_map(Weak::upgrade) {
            slot.close();
        }
    }
}

impl fmt::Debug for EventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventBus")
            .field("capacity", &self.shared.capacity)
            .field("has_subscribers", &self.has_subscribers())
            .finish()
    }
}

impl fmt::Debug for EventSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let queue = lock(&self.slot.state);
        f.debug_struct("EventSubscription")
            .field("queued", &queue.events.len())
            .field("missed", &queue.missed)
            .finish_non_exhaustive()
    }
}

impl EventBus {
    /// Creates a bus that queues up to `capacity` events per slow subscriber (at least 1).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                capacity: capacity.max(1),
                slots: Mutex::default(),
            }),
        }
    }

    /// Sends an event to every current subscriber; events with no subscriber are dropped.
    pub fn publish(&self, event: Event) {
        let live: Vec<Arc<Slot>> = {
            let mut slots = lock(&self.shared.slots);
            let mut live = Vec::with_capacity(slots.len());
            slots.retain(|slot| slot.upgrade().map(|slot| live.push(slot)).is_some());
            live
        };
        let Some((last, others)) = live.split_last() else {
            return;
        };
        for slot in others {
            slot.push(event.clone(), self.shared.capacity);
        }
        last.push(event, self.shared.capacity);
    }

    /// Reports whether anyone subscribes, so publishers can skip building
    /// events nobody reads, such as a copy of every game log line.
    #[must_use]
    pub fn has_subscribers(&self) -> bool {
        lock(&self.shared.slots)
            .iter()
            .any(|slot| slot.strong_count() > 0)
    }

    /// Starts receiving events published from now on.
    #[must_use]
    pub fn subscribe(&self) -> EventSubscription {
        let slot = Arc::new(Slot::default());
        lock(&self.shared.slots).push(Arc::downgrade(&slot));
        EventSubscription { slot }
    }
}

impl EventSubscription {
    /// Waits for the next item, or returns `None` once every [`EventBus`]
    /// handle is dropped and the queued events are read.
    pub async fn recv(&mut self) -> Option<Received> {
        loop {
            let ready = self.slot.ready.notified();
            {
                let mut queue = lock(&self.slot.state);
                if queue.missed > 0 {
                    return Some(Received::Missed(std::mem::take(&mut queue.missed)));
                }
                if let Some(event) = queue.events.pop_front() {
                    return Some(Received::Event(event));
                }
                if queue.closed {
                    return None;
                }
            }
            ready.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EventBus, Received};
    use crate::{
        Event, GameLogLine, LogLevel, LogSource, OperationId, OperationOutcome, TransferKind,
        TransferSnapshot,
    };

    fn finished(op: OperationId) -> Event {
        Event::OperationFinished {
            op,
            outcome: OperationOutcome::Succeeded,
        }
    }

    fn transfer(op: OperationId, kind: TransferKind, files_done: u64) -> Event {
        Event::Transfer {
            op,
            kind,
            progress: TransferSnapshot {
                files_done,
                files_total: 100,
                bytes_done: files_done * 10,
                bytes_total: Some(1000),
            },
        }
    }

    fn step(op: OperationId, index: u32) -> Event {
        Event::LaunchStep {
            op,
            step: format!("step-{index}"),
            index,
            count: 9,
        }
    }

    fn log(op: OperationId, n: u32) -> Event {
        Event::GameLog {
            op,
            line: GameLogLine::plain(LogSource::Stdout, LogLevel::Info, format!("line {n}")),
        }
    }

    async fn drain(events: &mut super::EventSubscription) -> Vec<Received> {
        let mut seen = Vec::new();
        while let Ok(Some(item)) =
            tokio::time::timeout(std::time::Duration::from_millis(10), events.recv()).await
        {
            seen.push(item);
        }
        seen
    }

    #[tokio::test]
    async fn delivers_to_every_subscriber() {
        let bus = EventBus::new(4);
        let mut first = bus.subscribe();
        let mut second = bus.clone().subscribe();
        let op = OperationId::random();
        bus.publish(finished(op));
        assert_eq!(first.recv().await, Some(Received::Event(finished(op))));
        assert_eq!(second.recv().await, Some(Received::Event(finished(op))));
    }

    #[tokio::test]
    async fn reports_missed_events_and_closing() {
        let bus = EventBus::new(2);
        let mut slow = bus.subscribe();
        let op = OperationId::random();
        for _ in 0..5 {
            bus.publish(finished(op));
        }
        assert_eq!(slow.recv().await, Some(Received::Missed(3)));
        assert_eq!(slow.recv().await, Some(Received::Event(finished(op))));
        assert_eq!(slow.recv().await, Some(Received::Event(finished(op))));
        drop(bus);
        assert_eq!(slow.recv().await, None);
    }

    #[tokio::test]
    async fn progress_keeps_the_latest_totals_in_order() {
        let bus = EventBus::new(16);
        let mut slow = bus.subscribe();
        let op = OperationId::random();
        bus.publish(step(op, 0));
        for done in 0..50 {
            bus.publish(transfer(op, TransferKind::Assets, done));
            bus.publish(transfer(op, TransferKind::GameFiles, done));
        }
        bus.publish(step(op, 1));
        bus.publish(transfer(op, TransferKind::Assets, 100));
        let expected: Vec<_> = [
            step(op, 0),
            transfer(op, TransferKind::GameFiles, 49),
            step(op, 1),
            transfer(op, TransferKind::Assets, 100),
        ]
        .into_iter()
        .map(Received::Event)
        .collect();
        assert_eq!(drain(&mut slow).await, expected);
    }

    #[tokio::test]
    async fn a_full_queue_drops_log_copies_and_progress_before_other_events() {
        let bus = EventBus::new(4);
        let mut slow = bus.subscribe();
        let op = OperationId::random();
        bus.publish(step(op, 0));
        bus.publish(transfer(op, TransferKind::Assets, 1));
        for n in 0..100 {
            bus.publish(log(op, n));
        }
        bus.publish(step(op, 1));
        bus.publish(transfer(op, TransferKind::GameFiles, 1));
        bus.publish(finished(op));
        let seen = drain(&mut slow).await;
        assert_eq!(seen[0], Received::Missed(101));
        let events: Vec<_> = seen[1..]
            .iter()
            .map(|item| match item {
                Received::Event(event) => event.clone(),
                Received::Missed(count) => panic!("missed {count} again"),
            })
            .collect();
        assert_eq!(
            events,
            [
                step(op, 0),
                step(op, 1),
                transfer(op, TransferKind::GameFiles, 1),
                finished(op)
            ]
        );
    }

    #[tokio::test]
    async fn dropped_subscriptions_stop_counting() {
        let bus = EventBus::new(1);
        assert!(!bus.has_subscribers());
        let events = bus.subscribe();
        assert!(bus.has_subscribers());
        drop(events);
        assert!(!bus.has_subscribers());
        bus.publish(finished(OperationId::random()));
    }

    #[tokio::test]
    async fn a_waiting_reader_wakes_up() {
        let bus = EventBus::new(4);
        let mut events = bus.subscribe();
        let op = OperationId::random();
        let reader = tokio::spawn(async move { events.recv().await });
        tokio::task::yield_now().await;
        bus.publish(finished(op));
        assert_eq!(reader.await.unwrap(), Some(Received::Event(finished(op))));
    }

    #[test]
    fn publishing_without_subscribers_is_fine() {
        EventBus::new(1).publish(finished(OperationId::random()));
    }
}
