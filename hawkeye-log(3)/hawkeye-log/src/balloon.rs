//! Balloon: the burn pin follows the cutdown state and the failsafe; the radio sends telemetry
//! (position and air data only), counts pings as contact, and answers pokes (pong and state) and
//! ARM / FIRE / DISARM commands (state). The state is also sent every `STATE_PERIOD_S`.

use core::cell::{Cell, RefCell};

use critical_section::Mutex;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::Output;
use esp_hal::rng::Trng;
use esp_hal::rtc_cntl::Rtc;
use log::{info, trace, warn};

use crate::config::{STATE_PERIOD_S, TEAM_ID, TELEMETRY_PERIOD_MS};
use crate::cutdown::{BurnState, CommandOutcome, CutdownStateMachine, FailsafeReason};
use crate::failsafe::{FailsafeMonitor, FailsafeStatus};
use crate::ping;
use crate::poke::{self, Pong};
use crate::radio::{self, LoraRadio, ReceiveResult, PAYLOAD_MAX, RETRY_DELAY_MS};
use crate::sensors::{env, gps};
use crate::state::StatePacket;
use crate::telemetry::TelemetryPacket;

/// How often the burn task applies the state machine to the pin.
const BURN_TICK_MS: u64 = 20;
/// How often the failsafe conditions are checked.
const FAILSAFE_CHECK_MS: u64 = 1_000;

/// Shared by `burn_task` (timeouts, failsafe) and `balloon_task` (commands, contact).
static CUTDOWN: Mutex<RefCell<CutdownStateMachine>> = Mutex::new(RefCell::new(CutdownStateMachine::new()));

/// What the state reply reports about the failsafe; written by `burn_task`.
static FAILSAFE_STATUS: Mutex<Cell<FailsafeStatus>> =
    Mutex::new(Cell::new(FailsafeStatus { above_min_alt: false, contact_lost: false, reason: None }));

/// Milliseconds since this boot.
fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// The only code that drives the burn pin. It follows the state machine and nothing else,
/// independent of the radio. It also runs the failsafe monitor, which can start a burn on its own.
#[embassy_executor::task]
pub async fn burn_task(mut pin: Output<'static>, rtc: Rtc<'static>) {
    let mut was_on = false;
    let mut failsafe = FailsafeMonitor::new();
    let mut next_check_ms = now_ms();
    loop {
        if now_ms() >= next_check_ms {
            next_check_ms += FAILSAFE_CHECK_MS;
            let g = gps::now();
            let start_burn = critical_section::with(|cs| {
                let mut cd = CUTDOWN.borrow_ref_mut(cs);
                let start = failsafe.step(
                    now_ms(),
                    rtc.time_since_power_up().as_millis(),
                    g.alt_m(),
                    cd.last_contact_ms,
                );
                if start {
                    cd.force_fire(now_ms());
                }
                start
            });
            let before = critical_section::with(|cs| FAILSAFE_STATUS.borrow(cs).get());
            let status = failsafe.status();
            critical_section::with(|cs| FAILSAFE_STATUS.borrow(cs).set(status));
            if status.reason != before.reason {
                warn!("!!! FAILSAFE CUTDOWN TRIGGERED: {:?}", status.reason);
            }
            if start_burn {
                warn!("failsafe cutdown: burning");
            }
            if status.contact_lost != before.contact_lost {
                warn!("contact with the ground station {}", if status.contact_lost { "LOST" } else { "restored" });
            }
        }
        let on = critical_section::with(|cs| CUTDOWN.borrow_ref_mut(cs).tick(now_ms()));
        if on {
            pin.set_high();
        } else {
            pin.set_low();
        }
        if on != was_on {
            warn!("burn {}", if on { "ON" } else { "OFF" });
            was_on = on;
        }
        Timer::after(Duration::from_millis(BURN_TICK_MS)).await;
    }
}

/// How often the balloon looks for a state change while it listens.
const STATE_WATCH_MS: u32 = 250;
/// A state change (the end of a burn, the end of the arm window, a failsafe) is sent again this
/// many times, `STATE_REPEAT_GAP_MS` apart: it is sent only once on its own, and a lost packet
/// would leave the ground station on the old state (e.g. FIRING) until the next periodic one.
const STATE_REPEATS: u8 = 2;
const STATE_REPEAT_GAP_MS: u64 = 1_000;

/// What counts as a state change: the burn state or the failsafe reason.
type StateKey = (BurnState, Option<FailsafeReason>);

/// Sends the cutdown state: the answer to a command or a poke, when it changes, and every
/// `STATE_PERIOD_S`. Returns what was sent, so that the change is not announced twice.
async fn send_state(radio: &mut LoraRadio) -> StateKey {
    let p = critical_section::with(|cs| {
        let cd = CUTDOWN.borrow_ref(cs);
        StatePacket {
            team_id: TEAM_ID,
            state: cd.state,
            code: cd.code(),
            burns: cd.burns,
            failsafe_reason: FAILSAFE_STATUS.borrow(cs).get().reason,
        }
    });
    match radio.send(&p.encode()).await {
        Ok(()) => info!(
            "state sent: {:?} code {:08X} burns {} failsafe {:?}",
            p.state, p.code, p.burns, p.failsafe_reason
        ),
        Err(e) => warn!("state send failed: {:?}", e),
    }
    (p.state, p.failsafe_reason)
}

/// The state now, for comparison with the last one sent.
fn state_key() -> StateKey {
    critical_section::with(|cs| (CUTDOWN.borrow_ref(cs).state, FAILSAFE_STATUS.borrow(cs).get().reason))
}

/// Sends telemetry every `TELEMETRY_PERIOD_MS` and listens for pings, pokes and commands in
/// between. A poke is answered with a pong and the state; a command with the state. The state
/// also goes out on its own every `STATE_PERIOD_S` and as soon as it changes (burn state or
/// failsafe reason, so also the arm timeout and the end of a burn); a change is then repeated
/// `STATE_REPEATS` times.
pub async fn balloon_task(mut radio: LoraRadio, rng: Trng) -> ! {
    info!("role: BALLOON, telemetry every {} ms", TELEMETRY_PERIOD_MS);
    let mut seq: u16 = 0;
    let mut next_state = Instant::now() + Duration::from_secs(STATE_PERIOD_S);
    let mut announced = state_key();
    let mut repeats_left: u8 = 0;
    let mut next_repeat = Instant::now();
    loop {
        let g = gps::now();
        let env = env::environment();
        let t = TelemetryPacket {
            team_id: TEAM_ID,
            seq,
            fix: g.fix,
            lat_e6: g.lat_e6,
            lon_e6: g.lon_e6,
            alt_dm: g.alt_dm,
            env_valid: env.valid,
            pressure_pa: env.pressure_pa,
            temp_c100: env.temp_c100,
            humidity_c100: env.humidity_c100,
        };
        seq = seq.wrapping_add(1);
        match radio.send(&t.encode()).await {
            Ok(()) => info!(
                "telemetry #{} sent: fix {} {:.6} {:.6} alt {:.1} m, sats {} | {:.2} hPa {:.2} C {:.1} %RH",
                t.seq, t.fix, t.lat_e6 as f64 / 1e6, t.lon_e6 as f64 / 1e6, t.alt_dm as f32 / 10.0, g.sats,
                t.pressure_pa as f32 / 100.0, t.temp_c100 as f32 / 100.0, t.humidity_c100 as f32 / 100.0,
            ),
            Err(e) => warn!("telemetry send failed: {:?}", e),
        }
        if Instant::now() >= next_state {
            announced = send_state(&mut radio).await;
            next_state = Instant::now() + Duration::from_secs(STATE_PERIOD_S);
        }

        let end = Instant::now() + Duration::from_millis(TELEMETRY_PERIOD_MS);
        loop {
            let left = end.saturating_duration_since(Instant::now()).as_millis();
            if left < radio::LISTEN_MIN_SLICE_MS {
                break;
            }
            if state_key() != announced {
                announced = send_state(&mut radio).await; // the state changed by itself
                next_state = Instant::now() + Duration::from_secs(STATE_PERIOD_S);
                repeats_left = STATE_REPEATS;
                next_repeat = Instant::now() + Duration::from_millis(STATE_REPEAT_GAP_MS);
                continue;
            }
            if repeats_left > 0 && Instant::now() >= next_repeat {
                announced = send_state(&mut radio).await; // the same change again, in case it was lost
                repeats_left -= 1;
                next_state = Instant::now() + Duration::from_secs(STATE_PERIOD_S);
                next_repeat = Instant::now() + Duration::from_millis(STATE_REPEAT_GAP_MS);
                continue;
            }
            let mut buf = [0u8; PAYLOAD_MAX];
            match radio.receive(&mut buf, (left as u32).min(STATE_WATCH_MS)).await {
                Ok(ReceiveResult::Packet { len, rssi_dbm, snr_db }) => {
                    let data = &buf[..len];
                    if ping::is_ping(data) {
                        match ping::verify(data) {
                            Ok(n) => {
                                trace!("ping #{} received, RSSI {} dBm, SNR {} dB", n, rssi_dbm, snr_db);
                                critical_section::with(|cs| CUTDOWN.borrow_ref_mut(cs).contact(now_ms()));
                            }
                            Err(e) => warn!("ping rejected: {:?}", e),
                        }
                        continue;
                    }
                    if poke::is_poke(data) {
                        match poke::verify_poke(data) {
                            Some(id) => {
                                info!("poke #{} received, RSSI {} dBm, SNR {} dB: answering", id, rssi_dbm, snr_db);
                                critical_section::with(|cs| CUTDOWN.borrow_ref_mut(cs).contact(now_ms()));
                                let pong = Pong { id, rssi_dbm, snr_db: snr_db.clamp(-128.0, 127.0) as i8 };
                                if let Err(e) = radio.send(&pong.encode()).await {
                                    warn!("pong send failed: {:?}", e);
                                }
                                announced = send_state(&mut radio).await;
                            }
                            None => warn!("poke rejected (bad signature or team)"),
                        }
                        continue;
                    }
                    let outcome = critical_section::with(|cs| {
                        CUTDOWN.borrow_ref_mut(cs).handle(data, now_ms(), rng.random())
                    });
                    match outcome {
                        CommandOutcome::BadFormat => {} // not a command
                        CommandOutcome::WrongTeam | CommandOutcome::BadMac => warn!("command rejected: {:?}", outcome),
                        _ => {
                            warn!("command: {:?}", outcome);
                            announced = send_state(&mut radio).await; // the answer to a command
                        }
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
