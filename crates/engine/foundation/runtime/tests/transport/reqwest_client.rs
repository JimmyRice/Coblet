use super::ReqwestTransport;
use crate::UserAgent;

#[test]
fn builds_a_client() {
    let agent = UserAgent::new("coblet-test/0.1").unwrap();
    assert!(ReqwestTransport::new(&agent).is_ok());
}

#[cfg(feature = "online-tests")]
#[tokio::test]
#[ignore = "uses the network; run with --features online-tests -- --ignored"]
async fn fetches_the_mojang_manifest() {
    use crate::{HttpUrl, Request, Transport};

    let agent = UserAgent::new("coblet-test/0.1").unwrap();
    let transport = ReqwestTransport::new(&agent).unwrap();
    let url =
        HttpUrl::new("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json").unwrap();
    let response = transport.send(Request::get(url)).await.unwrap();
    assert!(response.is_success());
    let body = response.bytes(64 << 20).await.unwrap();
    assert!(body.starts_with(b"{"));
}
