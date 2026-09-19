use std::{
    fs::File,
    io::{self, Read},
};

/// Fill a small process-local secret from the operating system CSPRNG.
/// Usagi targets macOS/Linux loopback execution where `/dev/urandom` is
/// provided by the OS; callers decide whether failure is fatal or recoverable.
pub(crate) fn fill_os_random(bytes: &mut [u8]) -> io::Result<()> {
    File::open("/dev/urandom")?.read_exact(bytes)
}
