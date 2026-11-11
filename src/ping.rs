//! Signed ping, ground -> balloon, 12 bytes: `0xA5`, team id, ping number (u16 LE), MAC.
//! MAC = first 8 bytes of HMAC-SHA256(KEY, the first 4 bytes). No hardware access.
//!
//! The ground station numbers its pings 0, 1, 2, ... A valid ping only counts as contact (it resets
//! the lost-contact timer); it changes nothing else. The balloon does not check the number, so a
//! recorded ping can be replayed to fake contact.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::config::TEAM_ID;
use crate::key::KEY;

pub const PING_SIZE: usize = 12;
const PING_MAGIC: u8 = 0xA5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PingError {
    /// Not a ping packet.
    NotPing,
    WrongTeam,
    BadMac,
}

pub(crate) fn mac(signed: &[u8]) -> Hmac<Sha256> {
    let mut m = Hmac::<Sha256>::new_from_slice(&KEY).expect("HMAC takes any key length");
    m.update(signed);
    m
}

/// A signed ping with the number `seq`.
pub fn encode(seq: u16) -> [u8; PING_SIZE] {
    let mut f = [0u8; PING_SIZE];
    f[0] = PING_MAGIC;
    f[1] = TEAM_ID;
    f[2..4].copy_from_slice(&seq.to_le_bytes());
    let tag = mac(&f[..4]).finalize().into_bytes();
    f[4..].copy_from_slice(&tag[..8]);
    f
}

/// True if `data` looks like a ping (length and magic), valid or not.
pub fn is_ping(data: &[u8]) -> bool {
    data.len() == PING_SIZE && data[0] == PING_MAGIC
}

/// Checks the signature (constant time) first, then the team. Returns the ping number.
pub fn verify(data: &[u8]) -> Result<u16, PingError> {
    let f: &[u8; PING_SIZE] = data.try_into().map_err(|_| PingError::NotPing)?;
    if f[0] != PING_MAGIC {
        return Err(PingError::NotPing);
    }
    mac(&f[..4]).verify_truncated_left(&f[4..]).map_err(|_| PingError::BadMac)?;
    if f[1] != TEAM_ID {
        return Err(PingError::WrongTeam);
    }
    Ok(u16::from_le_bytes([f[2], f[3]]))
}
