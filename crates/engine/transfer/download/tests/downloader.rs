//! Downloader scenarios against a scripted transport.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use coblet_runtime::{
    ArtifactKind, BoxFuture, CancellationToken, Expectation, HttpUrl, MemoryBody, Request,
    Response, Sources, TokioStorage, Transport, TransportError,
};
use coblet_types::FileHash;

use crate::{
    Destination, DownloadError, DownloadOutcome, DownloadTask, Downloader, DownloaderConfig,
    UrlRewriter,
};

const ABCDEF_SHA1: &str = "1f8ac10f23c5b5bc1167bda84b833e5c057a77d2";

/// Answer to one request.
enum Reply {
    Ok(u16, Vec<(String, String)>, &'static str),
    Fail(fn() -> TransportError),
}

/// Transport that answers requests from a script and records what it was asked.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<(String, Option<u64>)>>,
}

impl Scripted {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::default(),
        })
    }

    fn seen(&self) -> Vec<(String, Option<u64>)> {
        self.seen.lock().unwrap().clone()
    }
}

impl Transport for Scripted {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        self.seen
            .lock()
            .unwrap()
            .push((request.url().to_string(), request.resume_offset()));
        let reply = self.replies.lock().unwrap().pop_front();
        Box::pin(async move {
            match reply {
                Some(Reply::Ok(status, headers, body)) => {
                    Ok(Response::new(status, headers, MemoryBody::new(body)))
                }
                Some(Reply::Fail(error)) => Err(error()),
                None => Ok(Response::new(404, Vec::new(), MemoryBody::default())),
            }
        })
    }
}

fn ok(body: &'static str) -> Reply {
    Reply::Ok(200, Vec::new(), body)
}

fn config() -> DownloaderConfig {
    DownloaderConfig {
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(5),
        ..DownloaderConfig::default()
    }
}

fn downloader(transport: Arc<Scripted>) -> Downloader {
    Downloader::new(transport, Arc::new(TokioStorage::new()), config())
}

fn url(path: &str) -> HttpUrl {
    HttpUrl::new(format!("https://example.com/{path}")).unwrap()
}

fn task(dest: &Path, sources: Sources) -> DownloadTask {
    DownloadTask::new(
        sources,
        Expectation::hash(FileHash::new("sha1", ABCDEF_SHA1).unwrap(), Some(6)),
        Destination::Path(dest.to_path_buf()),
        ArtifactKind::Library,
    )
}

async fn run(
    downloader: &Downloader,
    task: &DownloadTask,
) -> Result<DownloadOutcome, DownloadError> {
    downloader
        .download(task, &CancellationToken::new(), &|_, _| {})
        .await
}

#[tokio::test]
async fn downloads_verifies_and_skips_present_files() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("lib/a.jar");
    let transport = Scripted::new(vec![ok("abcdef")]);
    let downloader = downloader(Arc::clone(&transport));
    let task = task(&dest, Sources::single(url("a.jar")));
    assert_eq!(
        run(&downloader, &task).await.unwrap(),
        DownloadOutcome::Downloaded { bytes: 6 }
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"abcdef");
    assert!(!dir.path().join("lib/a.jar.part").exists());
    assert_eq!(
        run(&downloader, &task).await.unwrap(),
        DownloadOutcome::AlreadyPresent
    );
    assert_eq!(transport.seen().len(), 1);
}

#[tokio::test]
async fn resumes_a_partial_file_with_a_range_request() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.jar");
    std::fs::write(dir.path().join("a.jar.part"), "abc").unwrap();
    let transport = Scripted::new(vec![Reply::Ok(206, Vec::new(), "def")]);
    let downloader = downloader(Arc::clone(&transport));
    let outcome = run(&downloader, &task(&dest, Sources::single(url("a.jar"))))
        .await
        .unwrap();
    assert_eq!(outcome, DownloadOutcome::Downloaded { bytes: 3 });
    assert_eq!(std::fs::read(&dest).unwrap(), b"abcdef");
    assert_eq!(transport.seen()[0].1, Some(3));
}

#[tokio::test]
async fn restarts_when_the_server_ignores_the_range() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.jar");
    std::fs::write(dir.path().join("a.jar.part"), "xyz").unwrap();
    let transport = Scripted::new(vec![ok("abcdef")]);
    run(
        &downloader(transport),
        &task(&dest, Sources::single(url("a.jar"))),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"abcdef");
}

#[tokio::test]
async fn retries_transient_failures_then_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.jar");
    let retry_now = vec![("Retry-After".to_owned(), "0".to_owned())];
    let transport = Scripted::new(vec![
        Reply::Fail(|| TransportError::Timeout),
        Reply::Ok(503, retry_now, ""),
        Reply::Ok(500, Vec::new(), ""),
        ok("abcdef"),
    ]);
    let sources = Sources {
        primary: url("primary"),
        fallbacks: vec![url("mirror")],
    };
    run(&downloader(Arc::clone(&transport)), &task(&dest, sources))
        .await
        .unwrap();
    let urls: Vec<String> = transport.seen().into_iter().map(|(url, _)| url).collect();
    assert_eq!(
        urls,
        [
            "https://example.com/primary",
            "https://example.com/primary",
            "https://example.com/primary",
            "https://example.com/mirror",
        ]
    );
}

#[tokio::test]
async fn does_not_retry_not_found_and_reports_the_last_error() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Scripted::new(vec![Reply::Ok(404, Vec::new(), "")]);
    let error = run(
        &downloader(Arc::clone(&transport)),
        &task(&dir.path().join("a"), Sources::single(url("a"))),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        DownloadError::AllSourcesFailed { sources: 1, .. }
    ));
    assert_eq!(transport.seen().len(), 1);
}

#[tokio::test]
async fn rejects_corrupt_files_and_keeps_the_destination_empty() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.jar");
    let transport = Scripted::new(vec![ok("abcdeX"), ok("abcdeY"), ok("abcdef")]);
    let error = run(
        &downloader(Arc::clone(&transport)),
        &task(&dest, Sources::single(url("a"))),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("does not match"), "{error}");
    assert!(!dest.exists());
    assert!(!dir.path().join("a.jar.part").exists());
    assert_eq!(
        transport.seen().len(),
        2,
        "a mismatch is retried once per source"
    );
}

#[tokio::test]
async fn stops_when_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = downloader(Scripted::new(vec![ok("abcdef")]))
        .download(
            &task(&dir.path().join("a"), Sources::single(url("a"))),
            &cancel,
            &|_, _| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(error, DownloadError::Cancelled));
}

#[tokio::test]
async fn caches_by_content_and_links_to_destinations() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Scripted::new(vec![ok("abcdef")]);
    let downloader = Downloader::new(
        Arc::clone(&transport) as Arc<dyn Transport>,
        Arc::new(TokioStorage::new()),
        DownloaderConfig {
            cache_root: Some(dir.path().join("cache")),
            ..config()
        },
    );
    let mut first = task(&dir.path().join("x"), Sources::single(url("a")));
    first.dest = Destination::ContentAddressed {
        link: Some(dir.path().join("one/mod.jar")),
    };
    let mut second = first.clone();
    second.dest = Destination::ContentAddressed {
        link: Some(dir.path().join("two/mod.jar")),
    };
    assert!(matches!(
        run(&downloader, &first).await.unwrap(),
        DownloadOutcome::Downloaded { .. }
    ));
    assert_eq!(
        run(&downloader, &second).await.unwrap(),
        DownloadOutcome::FromCache
    );
    assert_eq!(
        std::fs::read(dir.path().join("two/mod.jar")).unwrap(),
        b"abcdef"
    );
    assert!(
        dir.path()
            .join(format!("cache/objects/sha1/{ABCDEF_SHA1}"))
            .exists()
    );
    assert_eq!(transport.seen().len(), 1);
}

struct Mirror;

impl UrlRewriter for Mirror {
    fn rewrite(&self, url: &HttpUrl, _kind: &ArtifactKind) -> Option<HttpUrl> {
        let rest = url.as_str().strip_prefix("https://example.com/")?;
        HttpUrl::new(format!("https://mirror.example/{rest}")).ok()
    }
}

#[tokio::test]
async fn rewrites_addresses_and_runs_batches() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Scripted::new(vec![ok("abcdef"), ok("abcdef")]);
    let downloader = downloader(Arc::clone(&transport)).with_rewriter(Arc::new(Mirror));
    let tasks = vec![
        task(&dir.path().join("a"), Sources::single(url("a"))),
        task(&dir.path().join("b"), Sources::single(url("b"))),
    ];
    let results = downloader
        .download_all(tasks, &CancellationToken::new())
        .await;
    assert!(results.iter().all(Result::is_ok));
    assert!(
        transport
            .seen()
            .iter()
            .all(|(url, _)| url.starts_with("https://mirror.example/"))
    );
}

#[tokio::test]
async fn a_corrupt_mirror_falls_back_to_the_original_address() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.jar");
    // The mirror serves wrong bytes twice, then the original address answers.
    let transport = Scripted::new(vec![ok("abcdeX"), ok("abcdeX"), ok("abcdef")]);
    let downloader = downloader(Arc::clone(&transport)).with_rewriter(Arc::new(Mirror));
    run(&downloader, &task(&dest, Sources::single(url("a"))))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"abcdef");
    let urls: Vec<String> = transport.seen().into_iter().map(|(url, _)| url).collect();
    assert_eq!(
        urls,
        [
            "https://mirror.example/a",
            "https://mirror.example/a",
            "https://example.com/a"
        ]
    );

    let strict = Downloader::new(
        Arc::clone(&transport) as Arc<dyn Transport>,
        Arc::new(TokioStorage::new()),
        config(),
    )
    .with_rewriters(
        coblet_runtime::UrlRewriters::new()
            .with(Arc::new(Mirror))
            .without_fallback(),
    );
    transport
        .replies
        .lock()
        .unwrap()
        .extend([ok("abcdeX"), ok("abcdeX")]);
    let other = dir.path().join("b.jar");
    let error = run(&strict, &task(&other, Sources::single(url("b"))))
        .await
        .unwrap_err();
    assert!(
        matches!(error, DownloadError::AllSourcesFailed { sources: 1, .. }),
        "{error}"
    );
}

/// Transport that answers by path, handing out each path's replies in order.
struct ByPath(Mutex<std::collections::HashMap<&'static str, VecDeque<&'static str>>>);

impl Transport for ByPath {
    fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
        let path = request
            .url()
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let body = self
            .0
            .lock()
            .unwrap()
            .get_mut(path)
            .and_then(VecDeque::pop_front);
        Box::pin(async move {
            Ok(match body {
                Some(body) => Response::new(200, Vec::new(), MemoryBody::new(body)),
                None => Response::new(404, Vec::new(), MemoryBody::default()),
            })
        })
    }
}

#[tokio::test]
async fn batches_count_files_and_bytes_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("present"), "abcdef").unwrap();
    let replies = [
        ("fresh", VecDeque::from(["abcdef"])),
        // A corrupt first copy is taken back before the retry counts again.
        ("flaky", VecDeque::from(["abcxyz", "abcdef"])),
    ];
    let transport = Arc::new(ByPath(Mutex::new(replies.into_iter().collect())));
    let downloader = Downloader::new(transport, Arc::new(TokioStorage::new()), config());
    let tasks = ["present", "fresh", "flaky", "missing"]
        .map(|name| task(&dir.path().join(name), Sources::single(url(name))))
        .to_vec();
    let progress = coblet_runtime::TransferProgress::new();
    let results = downloader
        .download_all_with_progress(tasks, &CancellationToken::new(), Some(&progress))
        .await;
    assert_eq!(
        results[0].as_ref().unwrap(),
        &DownloadOutcome::AlreadyPresent
    );
    assert!(results[1].is_ok() && results[2].is_ok() && results[3].is_err());
    let totals = progress.snapshot();
    assert_eq!((totals.files_done, totals.files_total), (3, 4));
    assert_eq!((totals.bytes_done, totals.bytes_total), (18, Some(24)));
}
