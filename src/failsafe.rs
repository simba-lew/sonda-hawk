//! Failsafe cutdown: decides from GPS and ground contact alone when the balloon must cut itself
//! down. No hardware access. Nothing from the ground can cancel it; a ping only proves the ground
//! station is still there.
//!
//! It fires only if BOTH are true:
//!   1. contact is lost: no valid command or ping for FAILSAFE_LOST_CONTACT_S (a failed radio on
//!      either side ends up here too, because then no pings arrive)
//!   2. altitude is at least MIN_ALT_M (fresh fix, held for ALT_HOLD_S), OR the GPS has no fresh fix
//!      and FAILSAFE_GPS_FAILED_AFTER_S have passed since power-on
//!
//! It triggers once and starts a single burn. The state lives in RAM only: a reset starts the
//! monitor again from scratch.

use crate::config::{
    FAILSAFE_GPS_FAILED_AFTER_S, FAILSAFE_LOST_CONTACT_S, MIN_ALT_M,
};
use crate::cutdown::FailsafeReason;

/// A fix must stay above the minimum altitude this long (one bad GPS altitude must not count).
const ALT_HOLD_S: u64 = 5;

/// What the telemetry reports about the failsafe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FailsafeStatus {
    /// A fresh fix has been at or above MIN_ALT_M for ALT_HOLD_S.
    pub above_min_alt: bool,
    /// No valid command or ping for FAILSAFE_LOST_CONTACT_S.
    pub contact_lost: bool,
    /// Why the failsafe was triggered; None until it is.
    pub reason: Option<FailsafeReason>,
}

pub struct FailsafeMonitor {
    status: FailsafeStatus,
    /// Since when the altitude has been at or above MIN_ALT_M without a break.
    high_since_ms: Option<u64>,
}

impl Default for FailsafeMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl FailsafeMonitor {
    pub fn new() -> Self {
        Self { status: FailsafeStatus::default(), high_since_ms: None }
    }

    /// What the telemetry reports.
    pub fn status(&self) -> FailsafeStatus {
        self.status
    }

    /// Call about once a second. `now_ms` and `last_contact_ms` are on the same clock (since this
    /// boot); `since_start_ms` is the time since power-on. `alt_m` is the altitude if there is a
    /// fresh fix. Returns true once, when the failsafe triggers and the burn must be started.
    pub fn step(
        &mut self,
        now_ms: u64,
        since_start_ms: u64,
        alt_m: Option<i32>,
        last_contact_ms: u64,
    ) -> bool {
        let high = alt_m.is_some_and(|a| a >= MIN_ALT_M);
        self.high_since_ms = if high { self.high_since_ms.or(Some(now_ms)) } else { None };
        self.status.above_min_alt = self.high_since_ms.is_some_and(|t| now_ms - t >= ALT_HOLD_S * 1000);
        self.status.contact_lost = now_ms.saturating_sub(last_contact_ms) >= FAILSAFE_LOST_CONTACT_S * 1000;
        let gps_failed = alt_m.is_none() && since_start_ms >= FAILSAFE_GPS_FAILED_AFTER_S * 1000;

        if self.status.reason.is_some() || !self.status.contact_lost {
            return false;
        }
        self.status.reason = if self.status.above_min_alt {
            Some(FailsafeReason::LostContactHigh)
        } else if gps_failed {
            Some(FailsafeReason::LostContactNoGps)
        } else {
            None
        };
        self.status.reason.is_some()
    }
}
