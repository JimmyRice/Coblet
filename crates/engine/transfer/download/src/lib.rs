//! Downloads for coblet: every network fetch of game files, libraries,
//! assets, Java runtimes and content goes through [`Downloader`].
//!
//! A [`DownloadTask`] names its sources (a primary address and fallbacks),
//! the size and hashes the file must match, and a [`Destination`]: a fixed
//! path or the content-addressed cache. The downloader streams into a
//! `.part` file while hashing, renames the file into place only after it
//! verifies, resumes partial files with Range requests, retries with
//! backoff and switches sources after repeated failures. Requests are
//! bounded globally and per host. [`UrlRewriter`]s redirect addresses to
//! mirrors without relaxing verification; the trait lives in
//! `coblet-runtime` so metadata requests share it.
//!
//! [`Downloader`] implements [`coblet_runtime::DownloadHandler`], so an
//! [`coblet_runtime::Operations`] executor runs Plan download steps with it.
//!
//! ```
//! use std::sync::Arc;
//!
//! use coblet_download::{Destination, DownloadTask, Downloader, DownloaderConfig};
//! use coblet_runtime::{
//!     ArtifactKind, BoxFuture, CancellationToken, Expectation, HttpUrl, MemoryBody, Request,
//!     Response, Sources, TokioStorage, Transport, TransportError,
//! };
//! use coblet_types::FileHash;
//!
//! struct Fixed;
//!
//! impl Transport for Fixed {
//!     fn send(&self, _: Request) -> BoxFuture<'_, Result<Response, TransportError>> {
//!         Box::pin(async { Ok(Response::new(200, Vec::new(), MemoryBody::new("abc"))) })
//!     }
//! }
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let dir = tempfile::tempdir()?;
//! let downloader = Downloader::new(Arc::new(Fixed), Arc::new(TokioStorage::new()), DownloaderConfig::default());
//! let task = DownloadTask::new(
//!     Sources::single(HttpUrl::new("https://example.com/abc.txt")?),
//!     Expectation::hash(FileHash::new("sha1", "a9993e364706816aba3e25717850c26c9cd0d89d")?, Some(3)),
//!     Destination::Path(dir.path().join("abc.txt")),
//!     ArtifactKind::Library,
//! );
//! downloader.download(&task, &CancellationToken::new(), &|_, _| {}).await?;
//! assert_eq!(std::fs::read(dir.path().join("abc.txt"))?, b"abc");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
//! ```

#![forbid(unsafe_code)]

mod config;
mod downloader;
mod error;
mod limits;
mod retry;
mod task;
mod verify;

pub use coblet_runtime::{PrefixRewriter, Route, UrlRewriter, UrlRewriters};
pub use config::DownloaderConfig;
pub use downloader::{Downloader, ProgressFn};
pub use error::{DownloadError, FetchError};
pub use task::{Destination, DownloadOutcome, DownloadTask, Priority};
pub use verify::{FileIndex, VerifyMode};
