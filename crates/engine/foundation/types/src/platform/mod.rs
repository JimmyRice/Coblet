//! Platform description: operating system, CPU architecture, C library and free-form traits.

mod arch;
mod description;
mod libc;
mod name;
mod os;

pub use arch::Arch;
pub use description::Platform;
pub use libc::Libc;
pub use name::{ArchName, LibcName, OsName, PlatformNameError, PlatformTrait};
pub use os::OsFamily;
