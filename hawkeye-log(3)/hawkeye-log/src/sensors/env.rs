//! BME280 (pressure, temperature, humidity) on the sensor I2C bus; the latest reading goes into the
//! telemetry.
//!
//! Registers and formulas are from the Bosch BME280 datasheet; on a board it reads plausible
//! values (about 999 hPa, 23 degC indoors). The BME280 is only
//! specified for 30..110 kPa and -40..85 degC, so high up the pressure and temperature readings are
//! out of spec (still useful as a trend, not as an exact altitude).

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::{Duration, Timer};
use esp_hal::i2c::master::I2c;
use esp_hal::Blocking;
use log::{info, warn};

/// BME280 address with SDO high ...
const BME280: u8 = 0x77;
/// ... and with SDO low.
const BME280_FALLBACK: u8 = 0x76;
/// How often the BME280 is read for the telemetry.
const ENV_PERIOD_MS: u64 = 2_000;

/// Latest BME280 reading (`valid` is false until the first good read, or if the sensor fails).
#[derive(Clone, Copy, Default)]
pub struct Environment {
    pub valid: bool,
    pub pressure_pa: u32,
    pub temp_c100: i16,
    pub humidity_c100: u16,
}

static ENVIRONMENT: Mutex<Cell<Environment>> =
    Mutex::new(Cell::new(Environment { valid: false, pressure_pa: 0, temp_c100: 0, humidity_c100: 0 }));

/// The latest BME280 reading (`valid` is false if there is none).
pub fn environment() -> Environment {
    critical_section::with(|cs| ENVIRONMENT.borrow(cs).get())
}

type Bus = I2c<'static, Blocking>;

/// BME280 calibration values (datasheet table 16).
struct Calibration {
    t1: u16, t2: i16, t3: i16,
    p1: u16, p2: i16, p3: i16, p4: i16, p5: i16, p6: i16, p7: i16, p8: i16, p9: i16,
    h1: u8, h2: i16, h3: u8, h4: i16, h5: i16, h6: i8,
}

struct Bme280 {
    addr: u8,
    cal: Calibration,
}

impl Bme280 {
    /// None if no BME280 answers (a BMP280 has no humidity and is rejected too).
    fn new(i2c: &mut Bus) -> Option<Self> {
        for addr in [BME280, BME280_FALLBACK] {
            let mut id = [0u8];
            if i2c.write_read(addr, &[0xD0], &mut id).is_ok() && id[0] == 0x60 {
                let mut a = [0u8; 26]; // 0x88..0xA1
                let mut e = [0u8; 7]; // 0xE1..0xE7
                i2c.write_read(addr, &[0x88], &mut a).ok()?;
                i2c.write_read(addr, &[0xE1], &mut e).ok()?;
                let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
                let i16_at = |b: &[u8], i: usize| i16::from_le_bytes([b[i], b[i + 1]]);
                let cal = Calibration {
                    t1: u16_at(&a, 0), t2: i16_at(&a, 2), t3: i16_at(&a, 4),
                    p1: u16_at(&a, 6), p2: i16_at(&a, 8), p3: i16_at(&a, 10), p4: i16_at(&a, 12),
                    p5: i16_at(&a, 14), p6: i16_at(&a, 16), p7: i16_at(&a, 18), p8: i16_at(&a, 20),
                    p9: i16_at(&a, 22),
                    h1: a[25],
                    h2: i16_at(&e, 0),
                    h3: e[2],
                    h4: ((e[3] as i8 as i16) << 4) | (e[4] & 0x0F) as i16,
                    h5: ((e[5] as i8 as i16) << 4) | (e[4] >> 4) as i16,
                    h6: e[6] as i8,
                };
                return Some(Self { addr, cal });
            }
        }
        None
    }

    /// One forced-mode measurement (oversampling x1 for all three).
    async fn read(&self, i2c: &mut Bus) -> Option<Environment> {
        i2c.write(self.addr, &[0xF2, 0x01]).ok()?; // humidity x1 (takes effect with ctrl_meas)
        i2c.write(self.addr, &[0xF4, 0b0010_0101]).ok()?; // temp x1, pressure x1, forced mode
        Timer::after(Duration::from_millis(20)).await; // conversion takes ~10 ms
        let mut d = [0u8; 8];
        i2c.write_read(self.addr, &[0xF7], &mut d).ok()?;
        let adc_p = ((d[0] as i32) << 12) | ((d[1] as i32) << 4) | (d[2] as i32 >> 4);
        let adc_t = ((d[3] as i32) << 12) | ((d[4] as i32) << 4) | (d[5] as i32 >> 4);
        let adc_h = ((d[6] as i32) << 8) | d[7] as i32;
        if adc_t == 0x80000 || adc_p == 0x80000 {
            return None; // measurement skipped
        }
        let c = &self.cal;

        let var1 = (((adc_t >> 3) - ((c.t1 as i32) << 1)) * c.t2 as i32) >> 11;
        let dt = (adc_t >> 4) - c.t1 as i32;
        let var2 = (((dt * dt) >> 12) * c.t3 as i32) >> 14;
        let t_fine = var1 + var2;
        let temp_c100 = (t_fine * 5 + 128) >> 8;

        let mut v1 = t_fine as i64 - 128_000;
        let mut v2 = v1 * v1 * c.p6 as i64;
        v2 += (v1 * c.p5 as i64) << 17;
        v2 += (c.p4 as i64) << 35;
        v1 = ((v1 * v1 * c.p3 as i64) >> 8) + ((v1 * c.p2 as i64) << 12);
        v1 = (((1i64 << 47) + v1) * c.p1 as i64) >> 33;
        if v1 == 0 {
            return None;
        }
        let mut p = 1_048_576 - adc_p as i64;
        p = (((p << 31) - v2) * 3125) / v1;
        v1 = (c.p9 as i64 * (p >> 13) * (p >> 13)) >> 25;
        v2 = (c.p8 as i64 * p) >> 19;
        p = ((p + v1 + v2) >> 8) + ((c.p7 as i64) << 4);
        let pressure_pa = (p / 256) as u32;

        let mut h = (t_fine - 76_800) as i64;
        h = (((((adc_h as i64) << 14) - ((c.h4 as i64) << 20) - (c.h5 as i64 * h)) + 16_384) >> 15)
            * (((((((h * c.h6 as i64) >> 10) * (((h * c.h3 as i64) >> 11) + 32_768)) >> 10) + 2_097_152)
                * c.h2 as i64
                + 8_192)
                >> 14);
        h -= ((((h >> 15) * (h >> 15)) >> 7) * c.h1 as i64) >> 4;
        let h = h.clamp(0, 419_430_400) >> 12; // %RH in 1/1024
        let humidity_c100 = (h * 100 / 1024) as u16;

        Some(Environment { valid: true, pressure_pa, temp_c100: temp_c100 as i16, humidity_c100 })
    }
}

/// Reads the BME280 every `ENV_PERIOD_MS` into the value `environment()` returns.
#[embassy_executor::task]
pub async fn env_task(mut i2c: Bus) {
    let mut bme = Bme280::new(&mut i2c);
    if bme.is_none() {
        warn!("BME280 not found at 0x77 / 0x76 (check the sensor I2C bus in board.rs)");
    }
    let mut had_data = true;
    loop {
        if bme.is_none() {
            bme = Bme280::new(&mut i2c);
        }
        let env = match &bme {
            Some(b) => b.read(&mut i2c).await.unwrap_or_default(),
            None => Environment::default(),
        };
        if env.valid != had_data {
            had_data = env.valid;
            if env.valid {
                info!("BME280 data restored");
            } else {
                warn!("BME280 gave no data");
            }
        }
        critical_section::with(|cs| ENVIRONMENT.borrow(cs).set(env));
        Timer::after(Duration::from_millis(ENV_PERIOD_MS)).await;
    }
}
