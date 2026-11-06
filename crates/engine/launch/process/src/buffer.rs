use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use coblet_runtime::GameLogLine;
use tokio::sync::broadcast;

/// Default number of lines a session keeps.
pub const DEFAULT_LOG_CAPACITY: usize = 5_000;

/// Recent game log lines plus a live feed.
///
/// A subscriber first gets the lines still in the buffer, then every new
/// line, so a UI that opens late misses nothing that is still kept.
#[derive(Debug)]
pub struct LogBuffer {
    capacity: usize,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    lines: VecDeque<GameLogLine>,
    sender: Option<broadcast::Sender<GameLogLine>>,
}

/// Item of a [`LogStream`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogItem {
    /// A line.
    Line(GameLogLine),
    /// The subscriber fell behind and this many lines were skipped.
    Missed(u64),
}

/// History, then live lines, of a [`LogBuffer`].
#[derive(Debug)]
pub struct LogStream {
    history: VecDeque<GameLogLine>,
    live: Option<broadcast::Receiver<GameLogLine>>,
}

impl LogStream {
    /// Returns the next item, or `None` once the game ended and everything was read.
    pub async fn next(&mut self) -> Option<LogItem> {
        if let Some(line) = self.history.pop_front() {
            return Some(LogItem::Line(line));
        }
        let receiver = self.live.as_mut()?;
        match receiver.recv().await {
            Ok(line) => Some(LogItem::Line(line)),
            Err(broadcast::error::RecvError::Lagged(count)) => Some(LogItem::Missed(count)),
            Err(broadcast::error::RecvError::Closed) => {
                self.live = None;
                None
            }
        }
    }
}

impl LogBuffer {
    /// Creates a buffer that keeps the last `capacity` lines (at least one).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(capacity.min(4096));
        Self {
            capacity,
            inner: Mutex::new(Inner {
                lines: VecDeque::new(),
                sender: Some(sender),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a line and sends it to subscribers.
    pub fn push(&self, line: GameLogLine) {
        let mut inner = self.lock();
        if inner.lines.len() == self.capacity {
            inner.lines.pop_front();
        }
        inner.lines.push_back(line.clone());
        if let Some(sender) = &inner.sender {
            // No subscriber is not an error.
            let _ = sender.send(line);
        }
    }

    /// Returns the kept lines.
    #[must_use]
    pub fn snapshot(&self) -> Vec<GameLogLine> {
        self.lock().lines.iter().cloned().collect()
    }

    /// Subscribes: kept lines first, then live ones.
    #[must_use]
    pub fn subscribe(&self) -> LogStream {
        let inner = self.lock();
        LogStream {
            history: inner.lines.clone(),
            live: inner.sender.as_ref().map(broadcast::Sender::subscribe),
        }
    }

    /// Ends the live feed; subscribers finish after the lines already sent.
    pub fn close(&self) {
        self.lock().sender = None;
    }
}

#[cfg(test)]
#[path = "../tests/buffer.rs"]
mod tests;
