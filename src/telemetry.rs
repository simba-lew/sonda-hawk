//! Telemetry packet, balloon -> ground. 33 bytes, little-endian, no hardware access. Position and
//! air data only; the cutdown state is not here, the balloon sends it (see `state.rs`) as the
//! answer to a command or a poke.
//!
//!   0 magic `0xB7` | 1 team id | 2-3 sequence (u16) | 4 flags | 5-8 latitude (1e-6 deg)
//!   | 9-12 longitude | 13-16 altitude (dm) | 17-20 pressure (Pa) | 21-22 temperature
//!   (0.01 degC, i16) | 23-24 humidity (0.01 %RH) | 25-32 MAC: first 8 bytes of
//!   HMAC-SHA256(KEY, bytes 0..25). A packet with a wrong MAC is dropped.
//!
//! Flags: bit 0 = GPS fix, bit 1 = BME280 data valid (the others are unused).

use hmac::Mac;

use crate::ping::mac;

pub const TELEMETRY_MAGIC: u8 = 0xB7;
pub const TELEMETRY_SIZE: usize = 33;
/// Bytes covered by the MAC.
const SIGNED_SIZE: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TelemetryPacket {
    pub team_id: u8,
    pub seq: u16,
    pub fix: bool,
    pub lat_e6: i32,
    pub lon_e6: i32,
    pub alt_dm: i32,
    /// BME280 reading is valid (the three values below are 0 otherwise).
    pub env_valid: bool,
    pub pressure_pa: u32,
    pub temp_c100: i16,
    pub humidity_c100: u16,
}

impl TelemetryPacket {
    pub fn encode(&self) -> [u8; TELEMETRY_SIZE] {
        let mut o = [0u8; TELEMETRY_SIZE];
        o[0] = TELEMETRY_MAGIC;
        o[1] = self.team_id;
        o[2..4].copy_from_slice(&self.seq.to_le_bytes());
        o[4] = self.fix as u8 | (self.env_valid as u8) << 1;
        o[5..9].copy_from_slice(&self.lat_e6.to_le_bytes());
        o[9..13].copy_from_slice(&self.lon_e6.to_le_bytes());
        o[13..17].copy_from_slice(&self.alt_dm.to_le_bytes());
        o[17..21].copy_from_slice(&self.pressure_pa.to_le_bytes());
        o[21..23].copy_from_slice(&self.temp_c100.to_le_bytes());
        o[23..25].copy_from_slice(&self.humidity_c100.to_le_bytes());
        let tag = mac(&o[..SIGNED_SIZE]).finalize().into_bytes();
        o[SIGNED_SIZE..].copy_from_slice(&tag[..8]);
        o
    }

    /// None if the bytes are not a valid telemetry packet (length, magic, MAC).
    pub fn decode(b: &[u8]) -> Option<Self> {
        let b: &[u8; TELEMETRY_SIZE] = b.try_into().ok()?;
        if b[0] != TELEMETRY_MAGIC || mac(&b[..SIGNED_SIZE]).verify_truncated_left(&b[SIGNED_SIZE..]).is_err() {
            return None;
        }
        let i32_at = |i: usize| i32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Some(Self {
            team_id: b[1],
            seq: u16::from_le_bytes([b[2], b[3]]),
            fix: b[4] & 1 != 0,
            lat_e6: i32_at(5),
            lon_e6: i32_at(9),
            alt_dm: i32_at(13),
            env_valid: b[4] & 1 << 1 != 0,
            pressure_pa: u32::from_le_bytes([b[17], b[18], b[19], b[20]]),
            temp_c100: i16::from_le_bytes([b[21], b[22]]),
            humidity_c100: u16::from_le_bytes([b[23], b[24]]),
        })
    }
}
