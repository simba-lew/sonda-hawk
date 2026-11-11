//! Settings to review before every flight. Everything else (buffer sizes, task tick rates, GPS baud
//! rate, chip registers, ...) is a constant at the top of the file that uses it.

/// Packets of another team are ignored.
pub const TEAM_ID: u8 = 1;

// --- Cutdown ---

/// The failsafe only fires at or above this altitude (needs a fresh GPS fix).
pub const MIN_ALT_M: i32 = 10_000; // <- PLACEHOLDER
/// FIRE must follow ARM within this time.
pub const ARM_WINDOW_MS: u64 = 15_000;
/// How long the burn output stays active (3-5 s for the real wire).
pub const BURN_MS: u64 = 4_000; // <- PLACEHOLDER
/// GPIO number of the balloon's burn output (active HIGH). Must be a free pin.
pub const BURN_PIN: u8 = 38; // <- PLACEHOLDER

// --- Failsafe cutdown (the balloon cuts itself down; nothing from the ground can cancel it) ---
//
// It fires only if BOTH are true:
//   1. contact is lost: no valid command or ping for FAILSAFE_LOST_CONTACT_S. A failed radio on
//      either side ends up here too.
//   2. altitude is at least MIN_ALT_M (fresh fix), OR the GPS has failed and
//      FAILSAFE_GPS_FAILED_AFTER_S have passed since power-on.

/// Contact counts as lost after this long without a valid command or ping.
pub const FAILSAFE_LOST_CONTACT_S: u64 = 900; // <- PLACEHOLDER
/// Without a fresh GPS fix the altitude is unknown; the failsafe may then fire once this long has
/// passed since power-on. Longer than the wait on the pad plus the flight to MIN_ALT_M, or it can
/// fire too early.
pub const FAILSAFE_GPS_FAILED_AFTER_S: u64 = 7_200; // <- PLACEHOLDER

// --- Timing ---

/// The balloon sends telemetry this often (and listens for pings and commands in between).
/// Shorter = fresher position, more airtime.
pub const TELEMETRY_PERIOD_MS: u64 = 10_000; // <- PLACEHOLDER
/// The balloon sends its cutdown state this often on its own (it also answers every command and
/// poke with it).
pub const STATE_PERIOD_S: u64 = 30; // <- PLACEHOLDER
/// The ground station sends its ping this long after it has heard the balloon's telemetry (and
/// the state that may follow it at once): in the window in which the balloon listens, never at
/// the same time as the balloon's own packets. Longer than the airtime of both packets together
/// (about 0.15 s at SF7).
pub const GROUND_PING_DELAY_MS: u64 = 400;
/// A command or poke is sent at once, and again this often until the balloon answers. Shorter than
/// ARM_WINDOW_MS.
pub const GROUND_COMMAND_RETRY_MS: u64 = 2_000;
/// A command is sent again until the balloon's state confirms it, at most this many times.
pub const GROUND_MAX_TRIES: u8 = 10;
/// GPS data older than this does not count as a fix.
pub const GPS_MAX_AGE_MS: u64 = 5_000; // <- PLACEHOLDER
