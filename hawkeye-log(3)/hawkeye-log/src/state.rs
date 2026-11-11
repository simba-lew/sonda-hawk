//! State packet, balloon -> ground. No hardware access. The balloon sends it as the answer
//! to a command (ARM / FIRE / DISARM) or to a poke, on its own every `STATE_PERIOD_S` and when it changes; the telemetry carries only
//! position and air data.
//!
//! 17 bytes, little-endian: `0xBA`, team id, burn state (0 Safe, 1 Armed, 2 Firing), one-time code
//! (u32, non-zero only while ARMED), burn count, failsafe reason (0 none), MAC (first 8 bytes of
//! HMAC-SHA256(KEY, bytes 0..9)). A packet with a wrong MAC is dropped.
//!
//! The packet has no counter, so a recorded one can be replayed to the ground station. It only
//! shows an old state until the next answer.

use hmac::Mac;

use crate::cutdown::{BurnState, FailsafeReason};
use crate::ping::mac;

pub const STATE_MAGIC: u8 = 0xBA;
pub const STATE_SIZE: usize = 17;
/// Bytes covered by the MAC.
const SIGNED_SIZE: usize = 9;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatePacket {
    pub team_id: u8,
    pub state: BurnState,
    pub code: u32,
    pub burns: u8,
    /// Set once the failsafe cutdown has been started.
    pub failsafe_reason: Option<FailsafeReason>,
}

impl StatePacket {
    pub fn encode(&self) -> [u8; STATE_SIZE] {
        let mut o = [0u8; STATE_SIZE];
        o[0] = STATE_MAGIC;
        o[1] = self.team_id;
        o[2] = self.state as u8;
        o[3..7].copy_from_slice(&self.code.to_le_bytes());
        o[7] = self.burns;
        o[8] = self.failsafe_reason.map_or(0, |r| r as u8);
        let tag = mac(&o[..SIGNED_SIZE]).finalize().into_bytes();
        o[SIGNED_SIZE..].copy_from_slice(&tag[..8]);
        o
    }

    /// None if the bytes are not a valid state packet (length, magic, MAC, state).
    pub fn decode(b: &[u8]) -> Option<Self> {
        let b: &[u8; STATE_SIZE] = b.try_into().ok()?;
        if b[0] != STATE_MAGIC || mac(&b[..SIGNED_SIZE]).verify_truncated_left(&b[SIGNED_SIZE..]).is_err() {
            return None;
        }
        Some(Self {
            team_id: b[1],
            state: BurnState::from_u8(b[2])?,
            code: u32::from_le_bytes([b[3], b[4], b[5], b[6]]),
            burns: b[7],
            failsafe_reason: FailsafeReason::from_u8(b[8]),
        })
    }
}
