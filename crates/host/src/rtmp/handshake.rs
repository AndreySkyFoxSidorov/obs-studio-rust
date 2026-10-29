//! RTMPv3 simple or Adobe HMAC-SHA256 handshake; no RTMPE/SWF verification.
//! Protocol reference: OBS32.2.2 plugins/obs-outputs/librtmp/handshake.h,
//! independently authored with the existing Rust SHA256 implementation.
use super::{Error, io_error};
use sha2::{Digest, Sha256};

pub(super) const SIZE: usize = 1536;
const DIGEST: usize = 32;
const FP: &[u8] = b"Genuine Adobe Flash Player 001";
const FMS: &[u8] = b"Genuine Adobe Flash Media Server 001";
const SUFFIX: [u8; 32] = [
    0xf0,0xee,0xc2,0x4a,0x80,0x68,0xbe,0xe8,0x2e,0x00,0xd0,0xd1,0x02,0x9e,0x7e,0x57,
    0x6e,0xec,0x5d,0x2d,0x29,0x80,0x6f,0xab,0x93,0xb8,0xe6,0x36,0xcf,0xeb,0x31,0xae,
];
fn hmac(key_parts: &[&[u8]], message: &[&[u8]]) -> [u8; DIGEST] {
    // RFC2104: the full FMS key is68 bytes and MUST be hashed before padding.
    let mut key = [0u8; 64];
    let key_len = key_parts.iter().map(|part| part.len()).sum::<usize>();
    if key_len > key.len() {
        let mut hash = Sha256::new();
        for part in key_parts { hash.update(part); }
        key[..DIGEST].copy_from_slice(&hash.finalize());
    } else {
        let mut at = 0;
        for part in key_parts { key[at..at + part.len()].copy_from_slice(part); at += part.len(); }
    }
    let mut inner_pad = key;
    let mut outer_pad = key;
    for value in &mut inner_pad { *value ^= 0x36; }
    for value in &mut outer_pad { *value ^= 0x5c; }
    let mut inner = Sha256::new(); inner.update(inner_pad);
    for part in message { inner.update(part); }
    let mut outer = Sha256::new(); outer.update(outer_pad); outer.update(inner.finalize());
    outer.finalize().into()
}
fn equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0u8, |v, (a,b)| v | (a ^ b)) == 0
}
fn offset(packet: &[u8; SIZE], scheme: usize) -> usize {
    let index = if scheme == 0 { 8 } else { 772 };
    index + 4 + packet[index..index + 4].iter().map(|v| usize::from(*v)).sum::<usize>() % 728
}
fn random(bytes: &mut [u8]) -> Result<(), Error> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
        let status = unsafe { BCryptGenRandom(BCRYPT_ALG_HANDLE::default(), bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if status.0 < 0 {
            return Err(io_error("RTMP handshake nonce", std::io::Error::other(format!("BCryptGenRandom: {:#010x}",status.0 as u32))));
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(bytes))
            .map_err(|error| io_error("RTMP handshake nonce", error))
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    { let _ = bytes; Err(Error::Unsupported("RTMP handshake needs a system random source")) }
}
pub(super) struct Hello {
    pub(super) packet: [u8; SIZE],
    digest: [u8; DIGEST],
}
pub(super) struct Reply {
    pub(super) packet: [u8; SIZE],
    server_response_key: Option<[u8; DIGEST]>,
}
impl Hello {
    pub(super) fn new() -> Result<Self, Error> {
        let mut packet = [0; SIZE]; random(&mut packet[8..])?;
        // The first four bytes are a valid zero client uptime. A nonzero player
        // version requests the signed handshake, with simple-server fallback.
        packet[4..8].copy_from_slice(&[10,0,45,2]);
        let at = offset(&packet, 0);
        let digest = hmac(&[FP], &[&packet[..at], &packet[at + DIGEST..]]);
        packet[at..at + DIGEST].copy_from_slice(&digest);
        Ok(Self { packet, digest })
    }
    pub(super) fn reply(&self, server: &[u8; SIZE]) -> Result<Reply, Error> {
        if server[4..8] == [0; 4] {
            let mut packet = *server;
            // time2 is this client's receive time; its zero uptime is allowed.
            packet[4..8].fill(0);
            return Ok(Reply { packet, server_response_key: None });
        }
        let at = (0..=1).map(|scheme| offset(server, scheme)).find(|at| {
            equal(&hmac(&[FMS], &[&server[..*at], &server[*at + DIGEST..]]), &server[*at..*at + DIGEST])
        }).ok_or(Error::Protocol("signed RTMP server handshake digest mismatch"))?;
        let response_key = hmac(&[FP, &SUFFIX], &[&server[at..at + DIGEST]]);
        let mut packet = [0; SIZE]; random(&mut packet[..SIZE - DIGEST])?;
        let digest = hmac(&[&response_key], &[&packet[..SIZE - DIGEST]]);
        packet[SIZE - DIGEST..].copy_from_slice(&digest);
        let server_response_key = Some(hmac(&[FMS, &SUFFIX], &[&self.digest]));
        Ok(Reply { packet, server_response_key })
    }
    pub(super) fn verify(&self, reply: &Reply, server: &[u8; SIZE]) -> Result<(), Error> {
        let valid = if let Some(key) = &reply.server_response_key {
            equal(&hmac(&[key], &[&server[..SIZE - DIGEST]]), &server[SIZE - DIGEST..])
        } else {
            equal(&self.packet[..4], &server[..4]) && equal(&self.packet[8..], &server[8..])
        };
        if valid { Ok(()) } else { Err(Error::Protocol("RTMP server handshake response mismatch")) }
    }
}
