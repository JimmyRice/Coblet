use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use coblet_runtime::Secret;

use super::xuid;

#[test]
fn reads_the_xuid_claim() {
    let payload = URL_SAFE_NO_PAD.encode(br#"{"xuid":"2535400000000000","sub":"x"}"#);
    let token = Secret::new(format!("eyJhbGciOiJIUzI1NiJ9.{payload}.sig"));
    assert_eq!(xuid(&token).as_deref(), Some("2535400000000000"));
    assert_eq!(xuid(&Secret::new("opaque")), None);
}
