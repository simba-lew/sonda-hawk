//! Ground station: type `arm`, `fire`, `disarm` or `poke` in the serial monitor. A command is sent
//! at once and again every `GROUND_COMMAND_RETRY_MS` until the balloon's state reply shows the
//! effect (or the tries run out). `poke` is a manual ping that the balloon answers with a pong
//! and its state (see `poke.rs`, `state.rs`). Telemetry (position, air) arrives on its own and is
//! only logged; the state arrives as an answer to a command or a poke, and on its own every
//! `STATE_PERIOD_S` and on each change.
//!
//! Time slots: the periodic signed ping, which tells the balloon that the ground station is still
//! there, is sent `GROUND_PING_DELAY_MS` after the balloon's telemetry has been heard, i.e. in the
//! window in which the balloon listens, and never at the same time as its telemetry or state.
//! Without telemetry (not heard yet, or lost) the ping follows the balloon's rhythm by itself: it
//! repeats at the measured telemetry period, so it stays in its slot.

use embassy_time::{Duration, Instant, Timer};
use esp_hal::rng::Trng;
use log::{info, warn};

use crate::board::Console;
use crate::config::{
    ARM_WINDOW_MS, GROUND_COMMAND_RETRY_MS, GROUND_MAX_TRIES, GROUND_PING_DELAY_MS, TELEMETRY_PERIOD_MS, TEAM_ID,
};
use crate::cutdown::{encode_command, BurnState, CommandKind};
use crate::ping;
use crate::poke::{self, Pong};
use crate::state::StatePacket;
use crate::radio::{LoraRadio, ReceiveResult, LISTEN_MIN_SLICE_MS, PAYLOAD_MAX, RETRY_DELAY_MS};
use crate::telemetry::TelemetryPacket;

/// The ground station listens in slices of at most this length, so typed commands are picked up
/// quickly.
const LISTEN_SLICE_MS: u64 = 250;

/// A command waiting for the balloon's state reply to confirm it.
struct PendingCommand {
    kind: CommandKind,
    arg: u32,
    tries: u8,
    /// Burn count in the last state reply when the command was typed (a FIRE raises it).
    burns_before: u8,
}

/// A poke waiting for the balloon's answer.
struct PendingPoke {
    id: u16,
    tries: u8,
    /// When the poke was last sent, for the round-trip time.
    sent_at: Instant,
}

/// Runs the ground station. The serial monitor (`console`) is polled between radio receives.
pub async fn ground_task(mut radio: LoraRadio, mut console: Console, rng: Trng) -> ! {
    info!("=== GROUND STATION ===  commands: arm | fire | disarm | poke   (type and press Enter)");
    let mut line = [0u8; 16];
    let mut len = 0;
    // The last state reply, and the time at which the balloon's own arm timeout ends.
    let mut last: Option<StatePacket> = None;
    let mut armed_until: Option<Instant> = None;
    let mut pending: Option<PendingCommand> = None;
    let mut poke_pending: Option<PendingPoke> = None;
    let mut ping_seq: u16 = 0;
    let mut failsafe_alarm_shown = false;
    // When the pending command or poke is due, and when the next ping is.
    let mut next_cmd_at = Instant::now();
    let mut next_ping_at = Instant::now();
    // When the last telemetry was heard, and the balloon's telemetry period as measured here.
    let mut last_tel_at: Option<Instant> = None;
    let mut period_ms = TELEMETRY_PERIOD_MS;
    loop {
        // 1. send what is due: the pending command (again), the poke, or the ping. Something
        // that is due within `LISTEN_MIN_SLICE_MS` counts as due, as the listening ends then.
        let slack = Duration::from_millis(LISTEN_MIN_SLICE_MS);
        let due = |t: Instant| Instant::now() + slack >= t;
        let sent = if pending.is_some() && due(next_cmd_at) {
            let p = pending.as_mut().expect("checked above");
            if p.tries >= GROUND_MAX_TRIES {
                warn!("!!! no confirmation of {:?} after {} tries. Check the telemetry.", p.kind, p.tries);
                pending = None;
                Ok(())
            } else {
                p.tries += 1;
                next_cmd_at = Instant::now() + Duration::from_millis(GROUND_COMMAND_RETRY_MS);
                radio.send(&encode_command(p.kind, p.arg)).await
            }
        } else if pending.is_none() && poke_pending.is_some() && due(next_cmd_at) {
            let k = poke_pending.as_mut().expect("checked above");
            if k.tries >= GROUND_MAX_TRIES {
                warn!(">>> no reply to poke #{} after {} tries", k.id, k.tries);
                poke_pending = None;
                Ok(())
            } else {
                k.tries += 1;
                k.sent_at = Instant::now();
                next_cmd_at = Instant::now() + Duration::from_millis(GROUND_COMMAND_RETRY_MS);
                radio.send(&poke::encode_poke(k.id)).await
            }
        } else if pending.is_none() && poke_pending.is_none() && due(next_ping_at) {
            // Without telemetry to follow, the next ping comes one telemetry period later.
            next_ping_at = Instant::now() + Duration::from_millis(period_ms);
            let buf = ping::encode(ping_seq);
            ping_seq = ping_seq.wrapping_add(1);
            radio.send(&buf).await
        } else {
            Ok(())
        };
        if let Err(e) = sent {
            warn!("send failed: {:?}", e);
        }

        // 2. listen until the next send is due; read the console between receives
        let mut send_now = false;
        loop {
            while let Some(c) = console.read_byte() {
                if c.is_ascii_graphic() && len < line.len() {
                    line[len] = c.to_ascii_lowercase();
                    len += 1;
                } else if c == b'\r' || c == b'\n' {
                    let request = match &line[..core::mem::take(&mut len)] {
                        b"" => None,
                        b"arm" => {
                            last = None; // wait for the reply with the new code
                            armed_until = None;
                            Some((CommandKind::Arm, rng.random()))
                        }
                        b"fire" => match last {
                            Some(s) if s.state == BurnState::Armed && armed_until.is_some() => {
                                Some((CommandKind::Fire, s.code))
                            }
                            _ => {
                                warn!(">>> NOT sending FIRE: balloon is not ARMED, or its arm window is over (type 'arm' first and wait for READY)");
                                None
                            }
                        },
                        b"disarm" => Some((CommandKind::Disarm, 0)),
                        b"poke" => {
                            let id = rng.random() as u16;
                            info!(">>> poke #{} requested", id);
                            poke_pending = Some(PendingPoke { id, tries: 0, sent_at: Instant::now() });
                            next_cmd_at = Instant::now();
                            send_now = true; // do not wait for a time slot
                            None
                        }
                        _ => {
                            info!("commands: arm (get a one-time code), fire (within {} s of READY), disarm, poke (ping with an answer)", ARM_WINDOW_MS / 1000);
                            None
                        }
                    };
                    if let Some((kind, arg)) = request {
                        info!(">>> {:?} requested", kind);
                        pending = Some(PendingCommand { kind, arg, tries: 0, burns_before: last.map_or(0, |s| s.burns) });
                        next_cmd_at = Instant::now();
                        send_now = true; // commands do not wait for a time slot
                    }
                }
            }

            // The balloon drops back to SAFE by itself when its arm window ends; the ground follows.
            if armed_until.is_some_and(|t| Instant::now() >= t) {
                armed_until = None;
                if let Some(s) = last.as_mut().filter(|s| s.state == BurnState::Armed) {
                    s.state = BurnState::Safe;
                    s.code = 0;
                    warn!("state {:?} code 00000000 burns {} failsafe {:?} | arm window over, the balloon timed out by itself", s.state, s.burns, s.failsafe_reason);
                }
            }

            let wake = if pending.is_some() || poke_pending.is_some() { next_cmd_at } else { next_ping_at };
            let left = wake.saturating_duration_since(Instant::now()).as_millis();
            if send_now || left < LISTEN_MIN_SLICE_MS {
                break;
            }
            let mut buf = [0u8; PAYLOAD_MAX];
            match radio.receive(&mut buf, left.min(LISTEN_SLICE_MS) as u32).await {
                Ok(ReceiveResult::Packet { len, rssi_dbm, snr_db }) => {
                    let data = &buf[..len];
                    if let Some(t) = TelemetryPacket::decode(data).filter(|t| t.team_id == TEAM_ID) {
                        info!(
                            "telemetry #{} fix {} {:.6} {:.6} alt {:.1} m | {:.2} hPa {:.2} C {:.1} %RH | RSSI {} dBm, SNR {} dB",
                            t.seq, t.fix, t.lat_e6 as f64 / 1e6, t.lon_e6 as f64 / 1e6, t.alt_dm as f32 / 10.0,
                            t.pressure_pa as f32 / 100.0, t.temp_c100 as f32 / 100.0, t.humidity_c100 as f32 / 100.0,
                            rssi_dbm, snr_db
                        );
                        // Follow the balloon: the next ping goes into its listening window. The
                        // period is measured from consecutive packets (a lost one gives a double gap).
                        let now = Instant::now();
                        if let Some(prev) = last_tel_at {
                            let gap = now.duration_since(prev).as_millis();
                            if gap >= TELEMETRY_PERIOD_MS * 9 / 10 && gap <= TELEMETRY_PERIOD_MS * 11 / 10 {
                                period_ms = gap;
                            }
                        }
                        last_tel_at = Some(now);
                        next_ping_at = now + Duration::from_millis(GROUND_PING_DELAY_MS);
                    } else if let Some(s) = StatePacket::decode(data).filter(|s| s.team_id == TEAM_ID) {
                        info!(
                            "state {:?} code {:08X} burns {} failsafe {:?} | RSSI {} dBm, SNR {} dB",
                            s.state, s.code, s.burns, s.failsafe_reason, rssi_dbm, snr_db
                        );
                        if let (Some(reason), false) = (s.failsafe_reason, failsafe_alarm_shown) {
                            warn!("!!! FAILSAFE CUTDOWN TRIGGERED by the balloon: {:?}", reason);
                            failsafe_alarm_shown = true;
                        }
                        // The arm window runs from the first ARMED reply (a new code starts a new one);
                        // later copies of the same state do not extend it.
                        armed_until = match (s.state, last) {
                            (BurnState::Armed, Some(old)) if old.state == BurnState::Armed && old.code == s.code => armed_until,
                            (BurnState::Armed, _) => Some(Instant::now() + Duration::from_millis(ARM_WINDOW_MS)),
                            _ => None,
                        };
                        last = Some(s);
                        let Some(p) = &pending else { continue };
                        let done = match p.kind {
                            CommandKind::Arm => s.state == BurnState::Armed,
                            CommandKind::Fire => s.state == BurnState::Firing || s.burns != p.burns_before,
                            CommandKind::Disarm => s.state == BurnState::Safe,
                        };
                        if done {
                            match p.kind {
                                CommandKind::Arm => warn!(
                                    "*** READY: balloon is ARMED, code {:08X}. Type 'fire' within {} s, or 'disarm'.",
                                    s.code, ARM_WINDOW_MS / 1000
                                ),
                                CommandKind::Fire => warn!("*** FIRE CONFIRMED: balloon reports burn #{}", s.burns),
                                CommandKind::Disarm => warn!("*** DISARM CONFIRMED: balloon is SAFE"),
                            }
                            pending = None;
                        } else if p.kind == CommandKind::Fire && s.state == BurnState::Safe {
                            warn!("!!! FIRE was not accepted: the balloon is SAFE (arm window over?). Type 'arm' again.");
                            pending = None;
                        }
                    } else if let Some(pong) = Pong::decode(data) {
                        match poke_pending.take_if(|k| k.id == pong.id) {
                            Some(k) => warn!(
                                "*** POKE reply #{} after {} ms: the balloon heard us at {} dBm, SNR {} dB; we heard it at {} dBm, SNR {} dB",
                                pong.id, k.sent_at.elapsed().as_millis(), pong.rssi_dbm, pong.snr_db, rssi_dbm, snr_db
                            ),
                            None => info!("ignored a pong for poke #{}", pong.id),
                        }
                    } else {
                        info!("ignored a {} byte packet", len);
                    }
                }
                Ok(ReceiveResult::Timeout) => {}
                Err(e) => {
                    warn!("receive failed: {:?}", e);
                    Timer::after(Duration::from_millis(RETRY_DELAY_MS)).await;
                }
            }
        }
    }
}
