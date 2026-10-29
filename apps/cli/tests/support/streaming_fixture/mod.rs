//! Test-only source reuse. No public host listener or production dependency.
//! Copy to apps/cli/tests/support/streaming_fixture/mod.rs as described in INTEGRATION.
pub(crate) use skylive_host::rtmp::Error;
fn io_error(operation: &'static str, error: std::io::Error) -> Error {
    Error::Io {
        operation,
        detail: error.to_string(),
    }
}
#[allow(
    dead_code,
    reason = "reuse the exact bounded RTMP wire helper; this fixture needs only part of its API"
)]
#[path = "../../../../../crates/host/src/rtmp/amf.rs"]
mod amf;
#[allow(
    dead_code,
    reason = "reuse the exact bounded RTMP wire helper; abort is tested in the host crate"
)]
#[path = "../../../../../crates/host/src/rtmp/chunk.rs"]
mod chunk;
#[path = "../../../../../crates/host/src/rtmp/streaming_test_support.rs"]
mod server;
pub(crate) use server::Loopback;
