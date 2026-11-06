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
