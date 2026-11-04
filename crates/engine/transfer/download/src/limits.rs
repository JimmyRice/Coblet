//! Global and per-host concurrency limits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use coblet_runtime::HttpUrl;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Semaphores bounding requests in flight, overall and per host.
#[derive(Debug)]
pub(crate) struct Limits {
    global: Arc<Semaphore>,
    per_host_limit: usize,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
}

/// Permits held while one request runs.
pub(crate) struct Permits {
    _global: Option<OwnedSemaphorePermit>,
    _host: Option<OwnedSemaphorePermit>,
}

/// Returns the `host[:port]` part of a URL, lower-cased.
pub(crate) fn host_of(url: &HttpUrl) -> String {
    let rest = url.as_str().split_once("://").map_or("", |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host.to_ascii_lowercase()
}

impl Limits {
    pub(crate) fn new(global: usize, per_host: usize) -> Self {
        Self {
            global: Arc::new(Semaphore::new(global)),
            per_host_limit: per_host,
            hosts: Mutex::new(HashMap::new()),
        }
    }

    /// Waits for a global slot and a slot for the URL's host.
    pub(crate) async fn acquire(&self, url: &HttpUrl) -> Permits {
        let host = {
            let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(
                hosts
                    .entry(host_of(url))
                    .or_insert_with(|| Arc::new(Semaphore::new(self.per_host_limit))),
            )
        };
        // The semaphores are never closed, so acquiring only fails in theory;
        // a failure would merely skip that limit.
        let host = host.acquire_owned().await.ok();
        let global = Arc::clone(&self.global).acquire_owned().await.ok();
        Permits {
            _global: global,
            _host: host,
        }
    }
}

#[cfg(test)]
mod tests {
    use coblet_runtime::HttpUrl;

    use super::{Limits, host_of};

    #[test]
    fn extracts_hosts() {
        let host = |url: &str| host_of(&HttpUrl::new(url).unwrap());
        assert_eq!(
            host("https://Libraries.Minecraft.net/a/b"),
            "libraries.minecraft.net"
        );
        assert_eq!(host("http://user@example.com:8080?x"), "example.com:8080");
    }

    #[tokio::test]
    async fn limits_requests_per_host() {
        let limits = Limits::new(4, 1);
        let a = HttpUrl::new("https://a.example/1").unwrap();
        let b = HttpUrl::new("https://b.example/1").unwrap();
        let _held = limits.acquire(&a).await;
        let other_host =
            tokio::time::timeout(std::time::Duration::from_millis(50), limits.acquire(&b)).await;
        assert!(other_host.is_ok());
        let same_host =
            tokio::time::timeout(std::time::Duration::from_millis(50), limits.acquire(&a)).await;
        assert!(same_host.is_err());
    }
}
