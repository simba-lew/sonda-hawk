//! GPS: reads NMEA sentences from the u-blox module into the latest position (`now()`), and
//! switches the module to its airborne navigation model.

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::{Duration, Instant};
use esp_hal::uart::Uart;
use esp_hal::Async;
use log::{info, warn};

use crate::config::GPS_MAX_AGE_MS;

/// How often the airborne message is sent again.
const AIRBORNE_RESEND_S: u64 = 60;

/// One GGA position. All zero when there is no fix.
#[derive(Clone, Copy, Default)]
pub struct GpsPosition {
    pub fix: bool,
    pub lat_e6: i32,
    pub lon_e6: i32,
    /// Altitude above mean sea level in decimetres.
    pub alt_dm: i32,
    /// Satellites used in the fix.
    pub sats: u8,
}

impl GpsPosition {
    /// Altitude in whole metres, if there is a fresh fix.
    pub fn alt_m(&self) -> Option<i32> {
        self.fix.then(|| self.alt_dm.div_euclid(10))
    }
}

/// The newest position and the time (ms since boot) it was received.
static LATEST: Mutex<Cell<(GpsPosition, u64)>> =
    Mutex::new(Cell::new((GpsPosition { fix: false, lat_e6: 0, lon_e6: 0, alt_dm: 0, sats: 0 }, 0)));

/// The latest position; `fix` is false if the data is older than `GPS_MAX_AGE_MS`.
pub fn now() -> GpsPosition {
    let (mut gps, at_ms) = critical_section::with(|cs| LATEST.borrow(cs).get());
    if Instant::now().as_millis() - at_ms > GPS_MAX_AGE_MS {
        gps.fix = false;
    }
    gps
}

/// Wraps `payload` in a UBX frame: sync 0xB5 0x62, class, id, length, payload, Fletcher checksum.
fn ubx_frame(class: u8, id: u8, payload: &[u8], out: &mut [u8; 48]) -> usize {
    let n = payload.len();
    out[..2].copy_from_slice(&[0xB5, 0x62]);
    out[2] = class;
    out[3] = id;
    out[4..6].copy_from_slice(&(n as u16).to_le_bytes());
    out[6..6 + n].copy_from_slice(payload);
    let (mut a, mut b) = (0u8, 0u8);
    for &x in &out[2..6 + n] {
        a = a.wrapping_add(x);
        b = b.wrapping_add(a);
    }
    out[6 + n] = a;
    out[7 + n] = b;
    8 + n
}

/// Switches the GPS to the "airborne, < 1 g" navigation model (dynModel 6). Without it a u-blox
/// module limits its altitude in the default "portable" model (about 12 km). Two messages, because u-blox M10
/// modules want the new CFG-VALSET and older ones only know the legacy CFG-NAV5; the one that is
/// not understood is answered with a NAK and ignored.
async fn send_airborne_mode(uart: &mut Uart<'static, Async>) {
    let mut frame = [0u8; 48];

    // UBX-CFG-VALSET (0x06 0x8A): version 0, layers RAM + battery-backed, key CFG-NAVSPG-DYNMODEL
    // (0x20110021), value 6
    let mut valset = [0u8; 9];
    valset[1] = 0b11;
    valset[4..8].copy_from_slice(&0x2011_0021u32.to_le_bytes());
    valset[8] = 6;
    let n = ubx_frame(0x06, 0x8A, &valset, &mut frame);
    let r1 = uart.write_async(&frame[..n]).await;

    // UBX-CFG-NAV5 (0x06 0x24), 36 bytes: mask bit 0 = apply dynModel, dynModel = 6
    let mut nav5 = [0u8; 36];
    nav5[0] = 0x01;
    nav5[2] = 6;
    let n = ubx_frame(0x06, 0x24, &nav5, &mut frame);
    let r2 = uart.write_async(&frame[..n]).await;

    match (r1, r2) {
        (Ok(_), Ok(_)) => info!("GPS: airborne mode (dynModel 6) sent"),
        _ => warn!("GPS: could not send the airborne mode message (check the GPS wiring in board.rs)"),
    }
}

/// Reads the GPS UART for GGA sentences. The airborne message is sent a second after start-up (the
/// module needs time to boot), and again every `AIRBORNE_RESEND_S`, in case the module restarted.
#[embassy_executor::task]
pub async fn gps_task(mut uart: Uart<'static, Async>) {
    let mut next_airborne = Instant::now() + Duration::from_secs(1);
    let mut line = [0u8; 100];
    let mut len = 0;
    let mut buf = [0u8; 64];
    loop {
        if Instant::now() >= next_airborne {
            next_airborne = Instant::now() + Duration::from_secs(AIRBORNE_RESEND_S);
            send_airborne_mode(&mut uart).await;
        }
        let n = match uart.read_async(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                warn!("GPS UART error: {:?}", e);
                continue;
            }
        };
        for &b in &buf[..n] {
            match b {
                b'$' => (line[0], len) = (b, 1),
                b'\n' if len > 0 => {
                    if let Some(gps) = parse_gga(&line[..len]) {
                        let at_ms = Instant::now().as_millis();
                        critical_section::with(|cs| LATEST.borrow(cs).set((gps, at_ms)));
                    }
                    len = 0;
                }
                _ if len > 0 && len < line.len() => (line[len], len) = (b, len + 1),
                _ => {}
            }
        }
    }
}

/// "$GNGGA,time,lat,N,lon,E,quality,sats,hdop,alt,M,..*CS" (checksum verified).
fn parse_gga(line: &[u8]) -> Option<GpsPosition> {
    let text = core::str::from_utf8(line).ok()?.trim_end();
    let (body, sum) = text.strip_prefix('$')?.split_once('*')?;
    if body.bytes().fold(0, |a, b| a ^ b) != u8::from_str_radix(sum, 16).ok()? {
        return None;
    }
    let mut f = body.split(',');
    if !f.next()?.ends_with("GGA") {
        return None;
    }
    let (_time, lat, ns, lon, ew, quality, sats, _hdop, alt) =
        (f.next()?, f.next()?, f.next()?, f.next()?, f.next()?, f.next()?, f.next()?, f.next()?, f.next()?);
    if quality == "0" || quality.is_empty() {
        return Some(GpsPosition::default());
    }
    // ddmm.mmmm -> degrees * 1e6
    let coord = |s: &str, deg_digits: usize, neg: bool| -> Option<i32> {
        let deg: f64 = s.get(..deg_digits)?.parse().ok()?;
        let min: f64 = s.get(deg_digits..)?.parse().ok()?;
        let v = ((deg + min / 60.0) * 1e6) as i32;
        Some(if neg { -v } else { v })
    };
    Some(GpsPosition {
        fix: true,
        lat_e6: coord(lat, 2, ns == "S")?,
        lon_e6: coord(lon, 3, ew == "W")?,
        alt_dm: (alt.parse::<f64>().ok()? * 10.0) as i32,
        sats: sats.parse().ok()?,
    })
}
