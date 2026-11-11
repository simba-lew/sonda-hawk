//! Command packet (ground -> balloon) and the cutdown state machine. No hardware access.
//!
//! Command, 15 bytes: `0xC1`, team id, kind (1 ARM, 2 FIRE, 3 DISARM), argument (u32 LE), MAC.
//! MAC = first 8 bytes of HMAC-SHA256(KEY, the first 7 bytes). A command with a wrong MAC is
//! ignored. ARM: random request id. FIRE: the one-time code. DISARM: 0.
//!
//! Flow: ARM -> the balloon is ARMED and reports a fresh random code in its state reply.
//! FIRE(code) is accepted only with that code and inside the arm window. A new code on every
//! ARM, cleared after one use, makes a recorded FIRE worthless later.

use hmac::Mac;

use crate::config::{ARM_WINDOW_MS, BURN_MS, TEAM_ID};
use crate::ping::mac;

/// Bytes covered by the MAC.
const SIGNED_SIZE: usize = 7;
pub const COMMAND_SIZE: usize = SIGNED_SIZE + 8;
const COMMAND_MAGIC: u8 = 0xC1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BurnState {
    #[default]
    Safe = 0,
    Armed = 1,
    Firing = 2,
}

impl BurnState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Safe),
            1 => Some(Self::Armed),
            2 => Some(Self::Firing),
            _ => None,
        }
    }
}

/// Why the balloon started its failsafe cutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailsafeReason {
    /// Contact lost and altitude at least MIN_ALT_M.
    LostContactHigh = 1,
    /// Contact lost and the GPS failed for long enough since power-on.
    LostContactNoGps = 2,
}

impl FailsafeReason {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::LostContactHigh),
            2 => Some(Self::LostContactNoGps),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Arm = 1,
    Fire = 2,
    Disarm = 3,
}

/// A signed command.
pub fn encode_command(kind: CommandKind, arg: u32) -> [u8; COMMAND_SIZE] {
    let mut f = [0u8; COMMAND_SIZE];
    f[0] = COMMAND_MAGIC;
    f[1] = TEAM_ID;
    f[2] = kind as u8;
    f[3..SIGNED_SIZE].copy_from_slice(&arg.to_le_bytes());
    let tag = mac(&f[..SIGNED_SIZE]).finalize().into_bytes();
    f[SIGNED_SIZE..].copy_from_slice(&tag[..8]);
    f
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Not a command (another packet type).
    BadFormat,
    /// Wrong signature: not from our ground station.
    BadMac,
    WrongTeam,
    Armed,
    ArmRepeat,
    Firing,
    Busy,
    NotArmed,
    WrongCode,
    Disarmed,
}

/// The balloon's state. The caller feeds it frames and the clock, and drives the pin from `tick`.
pub struct CutdownStateMachine {
    pub state: BurnState,
    pub burns: u8,
    /// Time of the last valid command or ping. Boot = 0.
    pub last_contact_ms: u64,
    code: u32,
    req_id: u32,
    since_ms: u64,
}

impl Default for CutdownStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl CutdownStateMachine {
    pub const fn new() -> Self {
        Self { state: BurnState::Safe, burns: 0, last_contact_ms: 0, code: 0, req_id: 0, since_ms: 0 }
    }

    /// The code for the state reply: non-zero only while ARMED.
    pub fn code(&self) -> u32 {
        if self.state == BurnState::Armed { self.code } else { 0 }
    }

    fn safe(&mut self, now_ms: u64) {
        self.state = BurnState::Safe;
        self.code = 0;
        self.since_ms = now_ms;
    }

    /// Applies the timeouts. Returns true while the burn output must be active.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        let age = now_ms - self.since_ms;
        if (self.state == BurnState::Firing && age >= BURN_MS) || (self.state == BurnState::Armed && age > ARM_WINDOW_MS) {
            self.safe(now_ms);
        }
        self.state == BurnState::Firing
    }

    /// A valid ping or command arrived: the ground station is there.
    pub fn contact(&mut self, now_ms: u64) {
        self.last_contact_ms = now_ms;
    }

    /// Failsafe cutdown: burn now, without code or arm window (the balloon decided by itself).
    /// Does nothing if a burn is already running.
    pub fn force_fire(&mut self, now_ms: u64) {
        if self.state != BurnState::Firing {
            self.state = BurnState::Firing;
            self.code = 0;
            self.since_ms = now_ms;
            self.burns = self.burns.wrapping_add(1);
        }
    }

    /// Handles one received frame. `rnd` is a fresh random number.
    pub fn handle(&mut self, frame: &[u8], now_ms: u64, rnd: u32) -> CommandOutcome {
        let Ok(f) = <&[u8; COMMAND_SIZE]>::try_from(frame) else { return CommandOutcome::BadFormat };
        let kind = match (f[0], f[2]) {
            (COMMAND_MAGIC, 1) => CommandKind::Arm,
            (COMMAND_MAGIC, 2) => CommandKind::Fire,
            (COMMAND_MAGIC, 3) => CommandKind::Disarm,
            _ => return CommandOutcome::BadFormat,
        };
        if mac(&f[..SIGNED_SIZE]).verify_truncated_left(&f[SIGNED_SIZE..]).is_err() {
            return CommandOutcome::BadMac;
        }
        if f[1] != TEAM_ID {
            return CommandOutcome::WrongTeam;
        }
        let arg = u32::from_le_bytes([f[3], f[4], f[5], f[6]]);
        self.contact(now_ms);
        self.tick(now_ms);

        match (kind, self.state) {
            (CommandKind::Arm | CommandKind::Fire, BurnState::Firing) => CommandOutcome::Busy,
            (CommandKind::Arm, BurnState::Armed) if self.req_id == arg => CommandOutcome::ArmRepeat,
            (CommandKind::Arm, _) => {
                self.state = BurnState::Armed;
                self.code = rnd.max(1);
                self.req_id = arg;
                self.since_ms = now_ms;
                CommandOutcome::Armed
            }
            (CommandKind::Fire, BurnState::Safe) => CommandOutcome::NotArmed,
            (CommandKind::Fire, _) if arg != self.code => CommandOutcome::WrongCode,
            (CommandKind::Fire, _) => {
                self.state = BurnState::Firing;
                self.code = 0; // one use only
                self.since_ms = now_ms;
                self.burns = self.burns.wrapping_add(1);
                CommandOutcome::Firing
            }
            (CommandKind::Disarm, BurnState::Safe) => CommandOutcome::NotArmed,
            (CommandKind::Disarm, _) => {
                self.safe(now_ms); // also aborts a running burn
                CommandOutcome::Disarmed
            }
        }
    }
}
