//! The publisher owns exactly one network transport, including TLS retirement.
use super::{Error, ResolvedTarget, transport::Wire};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

pub(super) enum NetworkWire {
    Plain(TcpStream),
    #[cfg(target_os = "windows")]
    Tls(Box<super::tls_windows::TlsStream>),
}
impl NetworkWire {
    pub(super) fn open(
        socket: TcpStream,
        target: &ResolvedTarget,
        deadline: Instant,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        if let Some(host) = target.tls_server_name() {
            #[cfg(target_os = "windows")]
            return super::tls_windows::TlsStream::connect(socket, host, deadline, cancel)
                .map(|stream| Self::Tls(Box::new(stream)));
            #[cfg(not(target_os = "windows"))]
            {
                let _ = (socket, host, deadline, cancel);
                return Err(Error::Unsupported("RTMPS TLS adapter is available on Windows"));
            }
        }
        let _ = (deadline, cancel);
        Ok(Self::Plain(socket))
    }
}
impl Read for NetworkWire {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(bytes),
            #[cfg(target_os = "windows")]
            Self::Tls(stream) => stream.read(bytes),
        }
    }
}
impl Write for NetworkWire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(bytes),
            #[cfg(target_os = "windows")]
            Self::Tls(stream) => stream.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            #[cfg(target_os = "windows")]
            Self::Tls(stream) => stream.flush(),
        }
    }
}
impl Wire for NetworkWire {
    fn set_timeout(&mut self, value: Duration) -> io::Result<()> {
        match self {
            Self::Plain(stream) => Wire::set_timeout(stream, value),
            #[cfg(target_os = "windows")]
            Self::Tls(stream) => Wire::set_timeout(&mut **stream, value),
        }
    }
    fn ready(&mut self) -> io::Result<bool> {
        match self {
            Self::Plain(stream) => Wire::ready(stream),
            #[cfg(target_os = "windows")]
            Self::Tls(stream) => Wire::ready(&mut **stream),
        }
    }
    fn finish(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            #[cfg(target_os = "windows")]
            Self::Tls(stream) => Wire::finish(&mut **stream),
        }
    }
    fn physical_counts(&self) -> Option<(u64, u64)> {
        match self {
            Self::Plain(_) => None,
            #[cfg(target_os = "windows")]
            Self::Tls(stream) => stream.physical_counts(),
        }
    }
}
