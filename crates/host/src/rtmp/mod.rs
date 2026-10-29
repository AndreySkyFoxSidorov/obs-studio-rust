//! RTMP/Windows RTMPS publisher. No implicit network work.
//! See doc/migration/rtmp-publisher.md before integrating this module.
mod amf;
mod chunk;
mod flv;
mod handshake;
mod network;
#[cfg(target_os = "windows")]
mod tls_windows;
mod transport;
mod url;
mod worker;
pub use flv::{AudioConfig, AudioPacket, EncodedPacket, Generation, VideoConfig, VideoPacket};
use std::{fmt, io};
pub use transport::{ConnectOptions, Limits, Timeouts};
pub use url::ResolvedTarget;
pub use worker::{Event, PublishKey, Publisher, Stats};
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Unsupported(&'static str),
    Limit(&'static str),
    Protocol(&'static str),
    Rejected(String),
    ConnectionLost {operation:&'static str,detail:String},
    Io {
        operation: &'static str,
        detail: String,
    },
    Deadline(&'static str),
    Cancelled,
    Backpressure,
    Closed,
    StaleGeneration,
}
impl Error {
    pub fn retryable_transport(&self)->bool{matches!(self,Self::ConnectionLost{..}|Self::Deadline(_))}
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "rtmp_invalid",
            Self::Unsupported(_) => "rtmp_unsupported",
            Self::Limit(_) => "rtmp_limit",
            Self::Protocol(_) => "rtmp_protocol",
            Self::Rejected(_) => "rtmp_rejected",
            Self::ConnectionLost {..}=>"rtmp_connection_lost",
            Self::Io { .. } => "rtmp_io",
            Self::Deadline(_) => "rtmp_deadline",
            Self::Cancelled => "rtmp_cancelled",
            Self::Backpressure => "rtmp_backpressure",
            Self::Closed => "rtmp_closed",
            Self::StaleGeneration => "rtmp_stale_generation",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.code())?;
        match self {
            Self::Invalid(s)
            | Self::Unsupported(s)
            | Self::Limit(s)
            | Self::Protocol(s)
            | Self::Deadline(s) => f.write_str(s),
            Self::Rejected(s) => f.write_str(s),
            Self::ConnectionLost {operation,detail}|Self::Io { operation, detail } => write!(f, "{operation}: {detail}"),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for Error {}
pub(crate) fn io_error(operation: &'static str, error: io::Error) -> Error {
    if matches!(error.kind(),io::ErrorKind::ConnectionReset|io::ErrorKind::ConnectionAborted|io::ErrorKind::ConnectionRefused|io::ErrorKind::BrokenPipe|io::ErrorKind::NotConnected|io::ErrorKind::UnexpectedEof|io::ErrorKind::TimedOut|io::ErrorKind::NetworkDown|io::ErrorKind::NetworkUnreachable|io::ErrorKind::HostUnreachable){Error::ConnectionLost{operation,detail:error.to_string()}}else{Error::Io {operation,detail:error.to_string()}}
}
#[cfg(test)]
mod loopback_tests;
#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "windows"))]
pub(crate) mod streaming_test_support;

#[cfg(all(test, target_os = "windows"))]
mod native_stream_ui_witness;
