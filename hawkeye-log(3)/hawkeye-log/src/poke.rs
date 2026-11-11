//! Poke: a manual ping with an answer. No hardware access.
//!
//! Poke, ground -> balloon, 12 bytes: `0xA6`, team id, poke id (u16 LE), MAC.
//! Pong, balloon -> ground, 15 bytes: `0xB9`, team id, poke id (u16 LE, copied from the poke),
//! RSSI of the poke at the balloon (i16 LE, dBm), SNR (i8, dB), MAC.
//! MAC = first 8 bytes of HMAC-SHA256(KEY, all bytes before it).
//!
//! The ground station sends a poke with a fresh random id when asked and accepts only the pong
//! with that id. The balloon counts a valid poke as contact (like a ping) and answers at once.
//! Like pings, a recorded poke can be replayed, which only makes the balloon send a pong.

use hmac::Mac;

use crate::config::TEAM_ID;
use crate::ping::mac;

pub const POKE_SIZE: usize = 12;
const POKE_MAGIC: u8 = 0xA6;
pub const PONG_SIZE: usize = 15;
const PONG_MAGIC: u8 = 0xB9;
/// Bytes covered by the MAC.
const POKE_SIGNED: usize = 4;
const PONG_SIGNED: usize = 7;

/// A signed poke with the id `id`.
pub fn encode_poke(id: u16) -> [u8; POKE_SIZE] {
    let mut f = [0u8; POKE_SIZE];
    f[0] = POKE_MAGIC;
    f[1] = TEAM_ID;
    f[2..4].copy_from_slice(&id.to_le_bytes());
    let tag = mac(&f[..POKE_SIGNED]).finalize().into_bytes();
    f[POKE_SIGNED..].copy_from_slice(&tag[..8]);
    f
}

/// True if `data` looks like a poke (length and magic), valid or not.
pub fn is_poke(data: &[u8]) -> bool {
    data.len() == POKE_SIZE && data[0] == POKE_MAGIC
}

/// The poke id, if the signature and the team are right.
pub fn verify_poke(data: &[u8]) -> Option<u16> {
    let f: &[u8; POKE_SIZE] = data.try_into().ok()?;
    if f[0] != POKE_MAGIC || mac(&f[..POKE_SIGNED]).verify_truncated_left(&f[POKE_SIGNED..]).is_err() || f[1] != TEAM_ID {
        return None;
    }
    Some(u16::from_le_bytes([f[2], f[3]]))
}

/// The balloon's answer to a poke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pong {
    pub id: u16,
    /// How well the balloon heard the poke.
    pub rssi_dbm: i16,
    pub snr_db: i8,
}

impl Pong {
    pub fn encode(&self) -> [u8; PONG_SIZE] {
        let mut f = [0u8; PONG_SIZE];
        f[0] = PONG_MAGIC;
        f[1] = TEAM_ID;
        f[2..4].copy_from_slice(&self.id.to_le_bytes());
        f[4..6].copy_from_slice(&self.rssi_dbm.to_le_bytes());
        f[6] = self.snr_db as u8;
        let tag = mac(&f[..PONG_SIGNED]).finalize().into_bytes();
        f[PONG_SIGNED..].copy_from_slice(&tag[..8]);
        f
    }

    /// None if the bytes are not a valid pong of our team (length, magic, MAC, team).
    pub fn decode(data: &[u8]) -> Option<Self> {
        let f: &[u8; PONG_SIZE] = data.try_into().ok()?;
        if f[0] != PONG_MAGIC || mac(&f[..PONG_SIGNED]).verify_truncated_left(&f[PONG_SIGNED..]).is_err() || f[1] != TEAM_ID {
            return None;
        }
        Some(Self { id: u16::from_le_bytes([f[2], f[3]]), rssi_dbm: i16::from_le_bytes([f[4], f[5]]), snr_db: f[6] as i8 })
    }
}
