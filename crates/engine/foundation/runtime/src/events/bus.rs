use tokio::sync::broadcast;

use super::Event;

/// Fan-out channel for [`Event`]s; cloning gives another handle to the same channel.
///
/// Publishing never blocks. A subscriber that falls more than `capacity`
/// events behind skips the oldest ones and learns how many it missed.
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
#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

/// Receiving end of an [`EventBus`].
#[derive(Debug)]
pub struct EventSubscription {
    receiver: broadcast::Receiver<Event>,
}

/// Item delivered to a subscriber.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Received {
    /// The next event.
    Event(Event),
    /// The subscriber fell behind and this many events were dropped.
    Missed(u64),
}

impl EventBus {
    /// Creates a bus that buffers up to `capacity` events per slow subscriber (at least 1).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// Sends an event to every current subscriber; events with no subscriber are dropped.
    pub fn publish(&self, event: Event) {
        // An error only means nobody is subscribed right now.
        let _ = self.sender.send(event);
    }

    /// Reports whether anyone subscribes, so publishers can skip building
    /// events nobody reads, such as a copy of every game log line.
    #[must_use]
    pub fn has_subscribers(&self) -> bool {
        self.sender.receiver_count() > 0
    }

    /// Starts receiving events published from now on.
    #[must_use]
    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            receiver: self.sender.subscribe(),
        }
    }
}

impl EventSubscription {
    /// Waits for the next item, or returns `None` once every [`EventBus`] handle is dropped.
    pub async fn recv(&mut self) -> Option<Received> {
        match self.receiver.recv().await {
            Ok(event) => Some(Received::Event(event)),
            Err(broadcast::error::RecvError::Lagged(missed)) => Some(Received::Missed(missed)),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/events/bus.rs"]
mod tests;
