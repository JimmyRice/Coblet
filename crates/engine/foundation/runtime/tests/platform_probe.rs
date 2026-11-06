use coblet_types::OsFamily;

use super::{PlatformProbe, StdPlatformProbe};

#[test]
fn reports_the_compile_target() {
    let platform = StdPlatformProbe.detect().unwrap();
    assert_eq!(platform.os().as_str(), std::env::consts::OS);
    if cfg!(target_os = "macos") {
        assert_eq!(platform.os(), &OsFamily::MacOs);
        assert_eq!(platform.libc(), None);
    }
    assert_eq!(platform.os_version(), None);
}
