use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use coblet_runtime::{
    BoxFuture, CancellationToken, DownloadHandler, DownloadStep, EntryKind, Expectation, HttpUrl,
    Request, StepContext, StepError, StepOutcome, Storage, StorageError, StreamHasher,
    TransferProgress, Transport,
};
use coblet_types::FileHash;
use tokio::task::JoinSet;

use crate::limits::Limits;
use crate::retry::{backoff, retry_after};
use crate::verify::{FileIndex, verify_file};
use crate::{
    Destination, DownloadError, DownloadOutcome, DownloadTask, DownloaderConfig, FetchError,
    UrlRewriter, UrlRewriters, VerifyMode,
};

/// Progress callback: bytes of the current file received so far, and its size when known.
pub type ProgressFn<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

/// Fetches files over a [`Transport`] into a [`Storage`], verifying them on the way.
///
/// Each download streams into `<dest>.part` while hashing, checks size and
/// hashes, then renames the file into place, so a destination path only
/// ever holds a verified file. A `.part` left by an interrupted run is
/// resumed with a Range request when the size is known. Retryable failures
/// are repeated with exponential backoff (honoring `Retry-After`), then the
/// next source is tried.
///
/// The downloader also implements [`DownloadHandler`], so an
/// [`Operations`](coblet_runtime::Operations) executor uses it for
/// download steps. Cloning is cheap and shares limits and the file index.
#[derive(Clone)]
pub struct Downloader {
    transport: Arc<dyn Transport>,
    storage: Arc<dyn Storage>,
    config: DownloaderConfig,
    limits: Arc<Limits>,
    rewriters: UrlRewriters,
    index: Arc<FileIndex>,
}

/// Why one attempt stopped.
enum AttemptError {
    Cancelled,
    Storage(StorageError),
    Fetch(FetchError, Option<std::time::Duration>),
}

impl From<StorageError> for AttemptError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

/// Downloads one task, keeping `progress` in step with the bytes on disk.
async fn tracked(
    downloader: &Downloader,
    task: &DownloadTask,
    cancel: &CancellationToken,
    progress: &TransferProgress,
) -> Result<DownloadOutcome, DownloadError> {
    // Bytes of this file already counted; a new attempt reports from its
    // resume offset, which may be lower.
    let counted = AtomicU64::new(0);
    let report = |done: u64, _size: Option<u64>| {
        let before = counted.swap(done, Ordering::Relaxed);
        if done >= before {
            progress.add_bytes(done - before);
        } else {
            progress.remove_bytes(before - done);
        }
    };
    let result = downloader.download(task, cancel, &report).await;
    if result.is_ok() {
        // Present or cached files never report chunks; count their size.
        let counted = counted.load(Ordering::Relaxed);
        if let Some(size) = task.expect.size()
            && size > counted
        {
            progress.add_bytes(size - counted);
        }
        progress.file_done();
    }
    result
}

/// Status code and `Retry-After` value of a response, which is not `Sync` itself.
fn response_status(response: &coblet_runtime::Response) -> (u16, Option<String>) {
    (
        response.status(),
        response.header("retry-after").map(str::to_owned),
    )
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Returns `<algorithm>/<hex>` for the first hash the cache can be keyed by.
fn cache_key(expect: &Expectation) -> Option<String> {
    let Expectation::Hashes { primary, extra, .. } = expect else {
        return None;
    };
    std::iter::once(primary)
        .chain(extra)
        .find_map(|hash| match hash {
            FileHash::Sha1(digest) => Some(format!("sha1/{digest}")),
            FileHash::Sha256(digest) => Some(format!("sha256/{digest}")),
            FileHash::Sha512(digest) => Some(format!("sha512/{digest}")),
            FileHash::Other { .. } => None,
        })
}

impl Downloader {
    /// Creates a downloader without rewriters and with an empty file index.
    #[must_use]
    pub fn new(
        transport: Arc<dyn Transport>,
        storage: Arc<dyn Storage>,
        config: DownloaderConfig,
    ) -> Self {
        let limits = Arc::new(Limits::new(
            config.max_concurrent.get(),
            config.max_per_host.get(),
        ));
        Self {
            transport,
            storage,
            config,
            limits,
            rewriters: UrlRewriters::new(),
            index: Arc::new(FileIndex::new()),
        }
    }

    /// Adds a rewriter; rewriters run in the order they were added.
    #[must_use]
    pub fn with_rewriter(mut self, rewriter: Arc<dyn UrlRewriter>) -> Self {
        self.rewriters = self.rewriters.with(rewriter);
        self
    }

    /// Replaces the rewriter chain, for example with one shared with a metadata cache.
    #[must_use]
    pub fn with_rewriters(self, rewriters: UrlRewriters) -> Self {
        Self { rewriters, ..self }
    }

    /// Returns the rewriter chain.
    #[must_use]
    pub fn rewriters(&self) -> &UrlRewriters {
        &self.rewriters
    }

    /// Uses `index` for fast verification, for example one loaded from disk.
    #[must_use]
    pub fn with_index(self, index: Arc<FileIndex>) -> Self {
        Self { index, ..self }
    }

    /// Returns the file index, to save it after a batch of work.
    #[must_use]
    pub fn index(&self) -> &Arc<FileIndex> {
        &self.index
    }

    /// Returns the storage files are written to.
    #[must_use]
    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self.storage
    }

    /// Checks an existing file against `expect` without downloading.
    ///
    /// # Errors
    ///
    /// Returns the [`StorageError`] of reading the file.
    pub async fn verify(
        &self,
        path: &Path,
        expect: &Expectation,
        mode: VerifyMode,
    ) -> Result<bool, StorageError> {
        verify_file(&*self.storage, &self.index, path, expect, mode).await
    }

    /// Runs one task. A destination that already matches is not downloaded again.
    ///
    /// # Errors
    ///
    /// Returns [`DownloadError::Cancelled`] once `cancel` fires,
    /// [`DownloadError::AllSourcesFailed`] when no address delivered a
    /// matching file, [`DownloadError::Storage`] for local IO failures, and
    /// [`DownloadError::NoCacheKey`] / [`DownloadError::NoCacheRoot`] for
    /// content-addressed tasks that cannot be cached.
    pub async fn download(
        &self,
        task: &DownloadTask,
        cancel: &CancellationToken,
        progress: ProgressFn<'_>,
    ) -> Result<DownloadOutcome, DownloadError> {
        if !task.expect.is_verified() {
            tracing::warn!(url = %task.sources.primary, "downloading without a hash to check");
        }
        match &task.dest {
            Destination::Path(dest) => {
                if self.verify(dest, &task.expect, VerifyMode::Fast).await? {
                    return Ok(DownloadOutcome::AlreadyPresent);
                }
                self.fetch(task, dest, cancel, progress).await
            }
            Destination::ContentAddressed { link } => {
                let root = self
                    .config
                    .cache_root
                    .as_ref()
                    .ok_or(DownloadError::NoCacheRoot)?;
                let key = cache_key(&task.expect).ok_or(DownloadError::NoCacheKey)?;
                let object = key
                    .split('/')
                    .fold(root.join("objects"), |path, part| path.join(part));
                let outcome = if self.verify(&object, &task.expect, VerifyMode::Fast).await? {
                    DownloadOutcome::FromCache
                } else {
                    self.fetch(task, &object, cancel, progress).await?
                };
                if let Some(link) = link
                    && !self.verify(link, &task.expect, VerifyMode::Fast).await?
                {
                    self.place(&object, link).await?;
                }
                Ok(outcome)
            }
        }
    }

    /// Runs several tasks concurrently, starting higher priorities first.
    ///
    /// Results come back in the order of `tasks`. Concurrency is bounded by
    /// the configured global and per-host limits.
    pub async fn download_all(
        &self,
        tasks: Vec<DownloadTask>,
        cancel: &CancellationToken,
    ) -> Vec<Result<DownloadOutcome, DownloadError>> {
        self.download_all_with_progress(tasks, cancel, None).await
    }

    /// Like [`download_all`](Self::download_all), counting every file and
    /// received chunk in `progress`.
    ///
    /// All tasks are added to the totals before the first one starts. Files
    /// already present count as done with their full size. A retried
    /// attempt takes back the bytes of the attempt it replaces, so the
    /// counters never run ahead of the files on disk. Updates are relaxed
    /// atomic additions; [`TransferProgress::track`] turns them into events.
    pub async fn download_all_with_progress(
        &self,
        tasks: Vec<DownloadTask>,
        cancel: &CancellationToken,
        progress: Option<&TransferProgress>,
    ) -> Vec<Result<DownloadOutcome, DownloadError>> {
        if let Some(progress) = progress {
            for task in &tasks {
                progress.add_file(task.expect.size());
            }
        }
        let mut order: Vec<usize> = (0..tasks.len()).collect();
        order.sort_by_key(|&index| std::cmp::Reverse(tasks[index].priority));
        let tasks = Arc::new(tasks);
        let mut running = JoinSet::new();
        for index in order {
            let this = self.clone();
            let tasks = Arc::clone(&tasks);
            let cancel = cancel.clone();
            let progress = progress.cloned();
            running.spawn(async move {
                let task = &tasks[index];
                let result = match progress {
                    Some(progress) => tracked(&this, task, &cancel, &progress).await,
                    None => this.download(task, &cancel, &|_, _| {}).await,
                };
                (index, result)
            });
        }
        let mut results: Vec<Option<Result<DownloadOutcome, DownloadError>>> =
            std::iter::repeat_with(|| None).take(tasks.len()).collect();
        while let Some(joined) = running.join_next().await {
            if let Ok((index, result)) = joined
                && let Some(slot) = results.get_mut(index)
            {
                *slot = Some(result);
            }
        }
        results
            .into_iter()
            .map(|result| result.unwrap_or(Err(DownloadError::Cancelled)))
            .collect()
    }

    /// Links (or copies) a cached object to its destination.
    async fn place(&self, object: &Path, link: &Path) -> Result<(), StorageError> {
        let storage = &*self.storage;
        if let Some(existing) = storage.metadata(link).await?
            && existing.kind == EntryKind::File
        {
            storage.remove_file(link).await?;
        }
        // Hard links fail across file systems; a copy always works.
        if storage.hard_link(object, link).await.is_err() {
            storage.copy(object, link).await?;
        }
        Ok(())
    }

    /// Lists the addresses to try: the rewritten ones in source order, then
    /// the original addresses of those a rewriter changed.
    fn sources(&self, task: &DownloadTask) -> Vec<HttpUrl> {
        let routes: Vec<_> = task
            .sources
            .iter()
            .map(|url| self.rewriters.route(url, &task.kind))
            .collect();
        let mut sources: Vec<HttpUrl> = Vec::with_capacity(routes.len());
        let fallbacks = routes.iter().filter_map(|route| route.fallback.as_ref());
        for url in routes.iter().map(|route| &route.url).chain(fallbacks) {
            if !sources.contains(url) {
                sources.push(url.clone());
            }
        }
        sources
    }

    async fn fetch(
        &self,
        task: &DownloadTask,
        dest: &Path,
        cancel: &CancellationToken,
        progress: ProgressFn<'_>,
    ) -> Result<DownloadOutcome, DownloadError> {
        let sources = self.sources(task);
        let mut last = None;
        for url in &sources {
            let mut verify_retried = false;
            for attempt in 0..self.config.attempts_per_source.max(1) {
                if cancel.is_cancelled() {
                    return Err(DownloadError::Cancelled);
                }
                match self.attempt(url, task, dest, cancel, progress).await {
                    Ok(bytes) => return Ok(DownloadOutcome::Downloaded { bytes }),
                    Err(AttemptError::Cancelled) => return Err(DownloadError::Cancelled),
                    Err(AttemptError::Storage(error)) => return Err(error.into()),
                    Err(AttemptError::Fetch(error, wait)) => {
                        let is_verify = matches!(error, FetchError::Verify { .. });
                        // A second mismatch from the same address is not a transfer glitch.
                        let retry = error.is_retryable()
                            && attempt + 1 < self.config.attempts_per_source
                            && !(is_verify && verify_retried);
                        verify_retried |= is_verify;
                        tracing::debug!(%url, attempt, %error, retry, "download attempt failed");
                        last = Some(error);
                        if !retry {
                            break;
                        }
                        let wait = wait
                            .unwrap_or_else(|| {
                                backoff(attempt, self.config.backoff_base, self.config.backoff_max)
                            })
                            .min(self.config.backoff_max);
                        tokio::select! {
                            () = cancel.cancelled() => return Err(DownloadError::Cancelled),
                            () = tokio::time::sleep(wait) => {}
                        }
                    }
                }
            }
        }
        match last {
            Some(last) => Err(DownloadError::AllSourcesFailed {
                sources: sources.len(),
                last: Box::new(last),
            }),
            None => Err(DownloadError::Cancelled),
        }
    }

    /// One request to one address. Returns the bytes received.
    async fn attempt(
        &self,
        url: &HttpUrl,
        task: &DownloadTask,
        dest: &Path,
        cancel: &CancellationToken,
        progress: ProgressFn<'_>,
    ) -> Result<u64, AttemptError> {
        let storage = &*self.storage;
        let part = part_path(dest);
        let _permits = self.limits.acquire(url).await;
        let size = task.expect.size();
        let existing = storage
            .metadata(&part)
            .await?
            .filter(|metadata| metadata.kind == EntryKind::File)
            .map_or(0, |metadata| metadata.len);
        let resume = size
            .filter(|&size| existing > 0 && existing < size)
            .map(|_| existing);
        let mut request = Request::get(url.clone());
        if let Some(offset) = resume {
            request = request.resume_from(offset);
        }
        let fetch = |source| {
            AttemptError::Fetch(
                FetchError::Transport {
                    url: url.clone(),
                    source,
                },
                None,
            )
        };
        let mut response = tokio::select! {
            () = cancel.cancelled() => return Err(AttemptError::Cancelled),
            response = self.transport.send(request) => response.map_err(fetch)?,
        };
        let (mut writer, mut stream, start) = self
            .open_part(
                url,
                &response_status(&response),
                &part,
                resume,
                &task.expect,
            )
            .await?;
        let mut done = start;
        progress(done, size);
        loop {
            let chunk = tokio::select! {
                () = cancel.cancelled() => {
                    writer.finish().await?;
                    return Err(AttemptError::Cancelled);
                }
                chunk = response.next_chunk() => chunk,
            };
            let chunk = match chunk {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(source) => {
                    // Keep the partial file; the next attempt resumes it.
                    writer.finish().await?;
                    return Err(fetch(source));
                }
            };
            stream.update(&chunk);
            done += u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            writer.write(chunk).await?;
            progress(done, size);
            if size.is_some_and(|size| done > size) {
                break;
            }
        }
        writer.finish().await?;
        let hashes = stream.finish();
        if let Err(source) = hashes.check(&task.expect) {
            storage.remove_file(&part).await?;
            return Err(AttemptError::Fetch(
                FetchError::Verify {
                    url: url.clone(),
                    source,
                },
                None,
            ));
        }
        storage.rename(&part, dest).await?;
        if let Some(metadata) = storage.metadata(dest).await? {
            self.index.record(dest, &metadata, &hashes);
        }
        Ok(done - start)
    }

    /// Opens the `.part` file according to the response status: append after
    /// a 206 to a resume request, truncate after a 200, or fail.
    async fn open_part(
        &self,
        url: &HttpUrl,
        response: &(u16, Option<String>),
        part: &Path,
        resume: Option<u64>,
        expect: &Expectation,
    ) -> Result<(Box<dyn coblet_runtime::FileWriter>, StreamHasher, u64), AttemptError> {
        let storage = &*self.storage;
        let (status, retry_after_header) = response;
        let status = *status;
        let failed = |wait| {
            AttemptError::Fetch(
                FetchError::Status {
                    url: url.clone(),
                    status,
                },
                wait,
            )
        };
        match (status, resume) {
            (206, Some(offset)) => {
                // Hash what is already on disk so the final digest covers the whole file.
                let mut hasher = StreamHasher::for_expectation(expect);
                let mut reader = storage.open(part).await?;
                while let Some(chunk) = reader.next_chunk().await? {
                    hasher.update(&chunk);
                }
                Ok((storage.append(part).await?, hasher, offset))
            }
            (200, _) => Ok((
                storage.create(part).await?,
                StreamHasher::for_expectation(expect),
                0,
            )),
            (416, _) => {
                // The partial file no longer fits this resource; start over on the next attempt.
                storage.remove_file(part).await?;
                Err(failed(Some(std::time::Duration::ZERO)))
            }
            _ => Err(failed(
                retry_after_header
                    .as_deref()
                    .and_then(|value| retry_after(value, SystemTime::now())),
            )),
        }
    }
}

impl DownloadHandler for Downloader {
    fn download<'a>(
        &'a self,
        step: &'a DownloadStep,
        cx: &'a StepContext,
    ) -> BoxFuture<'a, Result<StepOutcome, StepError>> {
        Box::pin(async move {
            let dest = cx.resolve(&step.dest)?;
            let task = DownloadTask::new(
                step.sources.clone(),
                step.expect.clone(),
                Destination::Path(dest),
                step.kind.clone(),
            );
            let progress = |done, total| cx.report_progress(done, total);
            match Downloader::download(self, &task, cx.cancellation(), &progress).await {
                Ok(DownloadOutcome::Downloaded { .. }) => Ok(StepOutcome::Succeeded),
                Ok(DownloadOutcome::AlreadyPresent | DownloadOutcome::FromCache) => {
                    Ok(StepOutcome::Skipped)
                }
                Err(DownloadError::Cancelled) => Err(StepError::Cancelled),
                Err(DownloadError::Storage(error)) => Err(error.into()),
                Err(error) => Err(StepError::other(error)),
            }
        })
    }
}

impl std::fmt::Debug for Downloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downloader")
            .field("config", &self.config)
            .field("rewriters", &self.rewriters.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "../tests/downloader.rs"]
mod tests;
