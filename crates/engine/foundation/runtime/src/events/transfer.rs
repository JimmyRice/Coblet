use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{DEFAULT_PROGRESS_INTERVAL, Event, EventBus};
use crate::OperationId;

/// Group of files a transfer fetches.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransferKind {
    /// Libraries, the client jar, the asset index and the log configuration
    /// of a version.
    GameFiles,
    /// Asset objects: sounds, languages and textures.
    Assets,
    /// Files of a Java runtime.
    JavaRuntime,
    /// Anything else.
    Other,
}

impl TransferKind {
    /// Returns the stable name, such as `game-files`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GameFiles => "game-files",
            Self::Assets => "assets",
            Self::JavaRuntime => "java-runtime",
            Self::Other => "other",
        }
    }
}

/// Totals of a transfer at one moment.
///
/// Files already present count as done, with their size, as soon as they
/// are checked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TransferSnapshot {
    /// Files finished.
    pub files_done: u64,
    /// Files in the transfer.
    pub files_total: u64,
    /// Bytes received or found present.
    pub bytes_done: u64,
    /// Bytes in the transfer; `None` when a file's size is unknown.
    pub bytes_total: Option<u64>,
}

#[derive(Debug, Default)]
struct Counters {
    files_done: AtomicU64,
    files_total: AtomicU64,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    size_unknown: AtomicBool,
}

/// Counters of a batch transfer, shared between the tasks doing the work
/// and the code reporting it.
///
/// Updating costs one relaxed atomic operation and never blocks, so
/// downloads call it for every chunk. [`track`](Self::track) samples the
/// counters at most 30 times per second and publishes
/// [`Event::Transfer`] only when they changed and someone listens. Cloning
/// gives another handle to the same counters.
///
/// # Examples
///
/// ```
/// use coblet_runtime::TransferProgress;
///
/// let progress = TransferProgress::new();
/// progress.add_file(Some(100));
/// progress.add_file(Some(50));
/// progress.add_bytes(100);
/// progress.file_done();
/// let now = progress.snapshot();
/// assert_eq!((now.files_done, now.files_total), (1, 2));
/// assert_eq!((now.bytes_done, now.bytes_total), (100, Some(150)));
/// ```
#[derive(Clone, Debug, Default)]
pub struct TransferProgress(Arc<Counters>);

impl TransferProgress {
    /// Creates empty counters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a file to the total, with its size when known.
    pub fn add_file(&self, size: Option<u64>) {
        let counters = &self.0;
        counters.files_total.fetch_add(1, Ordering::Relaxed);
        match size {
            Some(size) => {
                counters.bytes_total.fetch_add(size, Ordering::Relaxed);
            }
            None => counters.size_unknown.store(true, Ordering::Relaxed),
        }
    }

    /// Counts received bytes.
    pub fn add_bytes(&self, bytes: u64) {
        self.0.bytes_done.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Takes back bytes counted for an attempt that is retried from the start.
    pub fn remove_bytes(&self, bytes: u64) {
        // Never goes below zero: callers only remove what they added.
        let _ = self
            .0
            .bytes_done
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |done| {
                Some(done.saturating_sub(bytes))
            });
    }

    /// Counts a finished file.
    pub fn file_done(&self) {
        self.0.files_done.fetch_add(1, Ordering::Relaxed);
    }

    /// Reads the counters.
    #[must_use]
    pub fn snapshot(&self) -> TransferSnapshot {
        let counters = &self.0;
        TransferSnapshot {
            files_done: counters.files_done.load(Ordering::Relaxed),
            files_total: counters.files_total.load(Ordering::Relaxed),
            bytes_done: counters.bytes_done.load(Ordering::Relaxed),
            bytes_total: (!counters.size_unknown.load(Ordering::Relaxed))
                .then(|| counters.bytes_total.load(Ordering::Relaxed)),
        }
    }

    /// Runs `work` while publishing [`Event::Transfer`] for these counters,
    /// at most 30 times per second, and once more with the final totals
    /// when the transfer had any files and the last event did not show
    /// them already.
    ///
    /// Without `events`, or while nobody subscribes, it only runs `work`.
    pub async fn track<F: Future>(
        &self,
        events: Option<&EventBus>,
        op: OperationId,
        kind: TransferKind,
        work: F,
    ) -> F::Output {
        let Some(events) = events else {
            return work.await;
        };
        let mut published = TransferSnapshot::default();
        let output = tokio::select! {
            output = work => output,
            never = self.sample(events, op, kind, &mut published) => match never {},
        };
        let last = self.snapshot();
        if last != published && last.files_total > 0 && events.has_subscribers() {
            events.publish(Event::Transfer {
                op,
                kind,
                progress: last,
            });
        }
        output
    }

    async fn sample(
        &self,
        events: &EventBus,
        op: OperationId,
        kind: TransferKind,
        published: &mut TransferSnapshot,
    ) -> Infallible {
        let mut ticks = tokio::time::interval(DEFAULT_PROGRESS_INTERVAL);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let now = self.snapshot();
            if now != *published && now.files_total > 0 && events.has_subscribers() {
                events.publish(Event::Transfer {
                    op,
                    kind,
                    progress: now,
                });
                *published = now;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{TransferKind, TransferProgress, TransferSnapshot};
    use crate::{Event, EventBus, OperationId, Received};

    #[test]
    fn counts_files_and_bytes() {
        let progress = TransferProgress::new();
        progress.add_file(Some(10));
        progress.add_file(None);
        progress.add_bytes(7);
        progress.remove_bytes(9);
        progress.add_bytes(4);
        progress.file_done();
        assert_eq!(
            progress.clone().snapshot(),
            TransferSnapshot {
                files_done: 1,
                files_total: 2,
                bytes_done: 4,
                bytes_total: None,
            }
        );
        assert_eq!(TransferKind::JavaRuntime.as_str(), "java-runtime");
    }

    #[tokio::test(start_paused = true)]
    async fn publishes_throttled_progress_and_the_final_totals() {
        let bus = EventBus::new(256);
        let op = OperationId::random();
        // One reader keeps up; the other reads only at the end and gets the
        // latest totals instead of a backlog.
        let mut late = bus.subscribe();
        let mut events = bus.subscribe();
        let reader = tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Some(Received::Event(event)) = events.recv().await {
                seen.push(event);
            }
            seen
        });
        let progress = TransferProgress::new();
        let work = {
            let progress = progress.clone();
            async move {
                for _ in 0..100 {
                    progress.add_file(Some(1));
                }
                // 100 files over one second: about 30 samples, not 100.
                for _ in 0..100 {
                    progress.add_bytes(1);
                    progress.file_done();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                42
            }
        };
        let output = progress
            .track(Some(&bus), op, TransferKind::Assets, work)
            .await;
        assert_eq!(output, 42);
        drop(bus);
        let mut seen = Vec::new();
        for event in reader.await.unwrap() {
            let Event::Transfer {
                op: from,
                kind,
                progress,
            } = event
            else {
                panic!("unexpected event {event:?}");
            };
            assert_eq!((from, kind), (op, TransferKind::Assets));
            seen.push(progress);
        }
        assert!((25..=40).contains(&seen.len()), "{} events", seen.len());
        assert!(seen.windows(2).all(|pair| pair[0] != pair[1]));
        let last = seen.last().unwrap();
        assert_eq!((last.files_done, last.bytes_done), (100, 100));
        assert_eq!(last.bytes_total, Some(100));
        let Some(Received::Event(Event::Transfer { progress, .. })) = late.recv().await else {
            panic!("the late reader got no transfer");
        };
        assert_eq!(&progress, last);
        assert_eq!(late.recv().await, None);
    }

    #[tokio::test]
    async fn stays_quiet_without_listeners_or_files() {
        let bus = EventBus::new(8);
        let progress = TransferProgress::new();
        let op = OperationId::random();
        progress
            .track(Some(&bus), op, TransferKind::Other, async {})
            .await;
        let mut events = bus.subscribe();
        progress
            .track(None, op, TransferKind::Other, async {})
            .await;
        progress
            .track(Some(&bus), op, TransferKind::Other, async {})
            .await;
        drop(bus);
        assert!(events.recv().await.is_none());
    }
}
