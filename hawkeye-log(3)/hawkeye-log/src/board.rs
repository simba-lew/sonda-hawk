//! The board: which pin does what, and how the peripherals are set up.
//! This is the only file that has to change for another board.
//!
//! The role decides the board:
//!
//! `--features ground`: Heltec WiFi LoRa 32 V3 / V3.2.
//!   radio  SCK 9, MISO 11, MOSI 10, NSS 8, RESET 12, BUSY 13, DIO1 14 (antenna switch on DIO2)
//!   console UART0 RX GPIO44 (the USB-UART bridge), burn pin GPIO35 (white LED, active HIGH)
//!
//! `--features balloon`: LilyGO T-Beam S3 Supreme.
//!   radio  SCK 12, MISO 13, MOSI 11, NSS 10, RESET 5, BUSY 4, DIO1 1 (antenna switch on DIO2)
//!   GPS    UART1, module TX -> GPIO9, module RX -> GPIO8 (u-blox), 9600 baud
//!   power  the radio (ALDO3) and the GPS (ALDO4) are switched on by the AXP2101 PMU (I2C SDA 42,
//!          SCL 41, address 0x34); without it the radio does not answer
//!   sensors BME280 on I2C SDA 17, SCL 18 (0x77)
//!   SD     SCK 36, MISO 37, MOSI 35, CS 47 (log files, see logfile.rs)
//!   burn   config::BURN_PIN

use esp_hal::clock::CpuClock;
#[cfg(feature = "balloon")]
use esp_hal::gpio::AnyPin;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig};
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use static_cell::StaticCell;

#[cfg(feature = "ground")]
use esp_hal::{uart::{Config as UartConfig, Uart, UartRx}, Blocking};
#[cfg(feature = "balloon")]
use embassy_time::{Duration, Timer};
#[cfg(feature = "balloon")]
use log::{error, info};
#[cfg(feature = "balloon")]
use esp_hal::{i2c::master::I2c, rtc_cntl::Rtc, uart::{Config as UartConfig, Uart}, Async, Blocking};

#[cfg(feature = "balloon")]
use crate::config::BURN_PIN;
#[cfg(feature = "balloon")]
use embassy_time::Delay;
#[cfg(feature = "balloon")]
use embedded_hal_bus::spi::ExclusiveDevice;
use crate::radio::{LoraRadio, LoraRadioError};

/// Serial monitor input, for typing commands: UART0 RX (the USB-UART bridge of the Heltec).
#[cfg(feature = "ground")]
pub struct Console(UartRx<'static, Blocking>);

#[cfg(feature = "ground")]
impl Console {
    /// The next typed byte, if there is one.
    pub fn read_byte(&mut self) -> Option<u8> {
        let mut b = [0u8];
        match self.0.read_buffered(&mut b) {
            Ok(1) => Some(b[0]),
            _ => None,
        }
    }
}

/// The balloon flashes the charge LED this many times at power-on (`FLASH_MS` on, `FLASH_MS` off).
#[cfg(feature = "balloon")]
const STARTUP_FLASHES: u8 = 5;
#[cfg(feature = "balloon")]
const FLASH_MS: u64 = 150;

/// The SD card on its own SPI bus (balloon only).
#[cfg(feature = "balloon")]
type SdSpi = ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, Delay>;

const SPI_MHZ: u32 = 2;
/// GPS module UART speed (the u-blox of the T-Beam works at 9600).
#[cfg(feature = "balloon")]
const GPS_BAUD: u32 = 9_600;

static TRNG_SOURCE: StaticCell<TrngSource<'static>> = StaticCell::new();

/// Everything the tasks need from the hardware.
pub struct BoardPeripherals {
    /// Burn output, already in its inactive level.
    pub burn_pin: Output<'static>,
    /// True random numbers (ADC entropy source is on for the whole run).
    pub rng: Trng,
    /// The radio may have failed to start (`main` stops with a message in that case).
    pub radio: Result<LoraRadio, LoraRadioError>,
    /// GPS module UART (balloon only): module TX -> GPIO9, module RX -> GPIO8.
    #[cfg(feature = "balloon")]
    pub gps_uart: Uart<'static, Async>,
    /// RTC clock: time since power-on, which keeps counting through resets (balloon only).
    #[cfg(feature = "balloon")]
    pub rtc: Rtc<'static>,
    /// Sensor bus with the BME280 (balloon only): SDA GPIO17, SCL GPIO18.
    #[cfg(feature = "balloon")]
    pub sensor_i2c: I2c<'static, Blocking>,
    /// Second CPU core (balloon only): the SD card runs there, see `main.rs`.
    #[cfg(feature = "balloon")]
    pub cpu_ctrl: esp_hal::peripherals::CPU_CTRL<'static>,
    #[cfg(feature = "balloon")]
    pub second_core_int: esp_hal::peripherals::FROM_CPU_INTR1<'static>,
    /// SD card, already set up as an SPI device (balloon only): SCK 36, MOSI 35, MISO 37, CS 47.
    #[cfg(feature = "balloon")]
    pub sd_spi: SdSpi,
    /// Serial monitor input, for typing commands (ground station only).
    #[cfg(feature = "ground")]
    pub console_rx: Console,
}

/// AXP2101 power management chip of the T-Beam S3 Supreme (LilyGO wiki, "AXP2101 Power Channels"):
/// ALDO3 powers the radio, ALDO4 the GPS and BLDO1 the SD card, all set to 3.3 V here. ALDO1
/// (BME280, display, magnetometer) and ALDO2 (a sensor) are 1.8 V / 3.3 V rails that are only
/// switched on, never changed. All of them are off until this runs.
#[cfg(feature = "balloon")]
async fn power_on_radio_and_gps(
    i2c0: esp_hal::peripherals::I2C0<'static>,
    sda: esp_hal::peripherals::GPIO42<'static>,
    scl: esp_hal::peripherals::GPIO41<'static>,
) {
    use esp_hal::i2c::master::Config as I2cConfig;

    const AXP2101: u8 = 0x34;
    const LDO_ENABLE: u8 = 0x90; // bit 0 = ALDO1, 1 = ALDO2, 2 = ALDO3, 3 = ALDO4, 4 = BLDO1
    const LDO_ON: u8 = 0b1_1111;
    const ALDO3_VOLTAGE: u8 = 0x94;
    const ALDO4_VOLTAGE: u8 = 0x95;
    /// The SD card rail. It was found at 1.8 V, at which a card does not answer at all.
    const BLDO1_VOLTAGE: u8 = 0x96;
    /// ALDO/BLDO voltage register value: (millivolt - 500) / 100.
    const V3_3: u8 = ((3300 - 500) / 100) as u8;

    let mut i2c = I2c::new(i2c0, I2cConfig::default()).expect("I2C config").with_sda(sda).with_scl(scl);
    let mut enable = [0u8];
    let result = i2c
        .write(AXP2101, &[ALDO3_VOLTAGE, V3_3])
        .and_then(|_| i2c.write(AXP2101, &[ALDO4_VOLTAGE, V3_3]))
        .and_then(|_| i2c.write(AXP2101, &[BLDO1_VOLTAGE, V3_3]))
        .and_then(|_| i2c.write_read(AXP2101, &[LDO_ENABLE], &mut enable))
        .and_then(|_| i2c.write(AXP2101, &[LDO_ENABLE, enable[0] | LDO_ON]));
    match result {
        Ok(()) => info!("PMU: radio, GPS and SD powered (LDO enable 0x{:02X})", enable[0] | LDO_ON),
        Err(e) => error!("PMU (AXP2101 at 0x34) did not answer: {:?}", e),
    }
    Timer::after(Duration::from_millis(100)).await; // rails settle
    flash_charge_led(&mut i2c, STARTUP_FLASHES).await;
}

/// Flashes the red charge LED, which the AXP2101 drives, `times` times, so a running board can
/// be recognised without the serial console. Then the LED goes back to its previous mode (charge
/// indicator). The register follows XPowersLib's `setChargingLedMode`: bit 0 = enable and bit 2 =
/// manual control (0x05), bits 5:4 = output (0 off, 3 on), bits 7:6 and 3 are kept.
#[cfg(feature = "balloon")]
async fn flash_charge_led(i2c: &mut I2c<'static, Blocking>, times: u8) {
    const AXP2101: u8 = 0x34;
    const CHGLED: u8 = 0x69;
    const MANUAL: u8 = 0x05;
    const LED_ON: u8 = 3 << 4;
    let mut original = [0u8];
    if let Err(e) = i2c.write_read(AXP2101, &[CHGLED], &mut original) {
        error!("PMU charge LED not readable, no start-up flashes: {:?}", e);
        return;
    }
    let manual = original[0] & 0xC8 | MANUAL;
    for _ in 0..times {
        let _ = i2c.write(AXP2101, &[CHGLED, manual | LED_ON]);
        Timer::after(Duration::from_millis(FLASH_MS)).await;
        let _ = i2c.write(AXP2101, &[CHGLED, manual]);
        Timer::after(Duration::from_millis(FLASH_MS)).await;
    }
    let _ = i2c.write(AXP2101, &[CHGLED, original[0]]);
}

/// Starts the chip and the async runtime's timer, and sets up the radio and the rest of the
/// hardware of the role.
pub async fn init() -> BoardPeripherals {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_rtos::start(TimerGroup::new(p.TIMG0).timer0, p.FROM_CPU_INTR0);

    // The burn output must be inactive from the very first moment (also on the ground station).
    let inactive = Level::Low; // active HIGH on both boards
    #[cfg(feature = "balloon")]
    // SAFETY: BURN_PIN is a free pin that nothing else in this file uses.
    let burn_pin = Output::new(unsafe { AnyPin::steal(BURN_PIN) }, inactive, OutputConfig::default());
    // The ground station has no cutdown; its burn pin is the Heltec white LED.
    #[cfg(feature = "ground")]
    let burn_pin = Output::new(p.GPIO35, inactive, OutputConfig::default());

    // Real random numbers for the one-time code need the ADC entropy source switched on and
    // kept alive for as long as the program runs, hence the static.
    TRNG_SOURCE.init(TrngSource::new(p.RNG, p.ADC1));
    let rng = Trng::try_new().expect("TRNG source is enabled above");

    // Heltec WiFi LoRa 32 V3: the SX1262 switches the antenna itself through DIO2, no RXEN pin.
    #[cfg(feature = "ground")]
    let radio = {
        let spi = Spi::new(p.SPI2, SpiConfig::default().with_frequency(Rate::from_mhz(SPI_MHZ)).with_mode(Mode::_0))
            .expect("SPI config")
            .with_sck(p.GPIO9)
            .with_miso(p.GPIO11)
            .with_mosi(p.GPIO10)
            .into_async();
        LoraRadio::new(
            spi,
            Output::new(p.GPIO8, Level::High, OutputConfig::default()),  // NSS
            Output::new(p.GPIO12, Level::High, OutputConfig::default()), // RESET
            Input::new(p.GPIO13, InputConfig::default()),                // BUSY
            Input::new(p.GPIO14, InputConfig::default()),                // DIO1
            None,
        )
        .await
    };
    // LilyGO T-Beam S3 Supreme: the PMU must power the radio and the GPS first. The SX1262
    // switches the antenna itself through DIO2, no RXEN pin.
    #[cfg(feature = "balloon")]
    power_on_radio_and_gps(p.I2C0, p.GPIO42, p.GPIO41).await;
    #[cfg(feature = "balloon")]
    let radio = {
        let spi = Spi::new(p.SPI2, SpiConfig::default().with_frequency(Rate::from_mhz(SPI_MHZ)).with_mode(Mode::_0))
            .expect("SPI config")
            .with_sck(p.GPIO12)
            .with_miso(p.GPIO13)
            .with_mosi(p.GPIO11)
            .into_async();
        LoraRadio::new(
            spi,
            Output::new(p.GPIO10, Level::High, OutputConfig::default()), // NSS
            Output::new(p.GPIO5, Level::High, OutputConfig::default()),  // RESET
            Input::new(p.GPIO4, InputConfig::default()),                 // BUSY
            Input::new(p.GPIO1, InputConfig::default()),                 // DIO1
            None,
        )
        .await
    };

    #[cfg(feature = "balloon")]
    let gps_uart = Uart::new(p.UART1, UartConfig::default().with_baudrate(GPS_BAUD))
        .expect("GPS UART config")
        .with_rx(p.GPIO9)
        .with_tx(p.GPIO8)
        .into_async();

    #[cfg(feature = "balloon")]
    let rtc = Rtc::new(p.RTC_TIMER);

    #[cfg(feature = "balloon")]
    // sensor bus: the BME280
    let sensor_i2c = I2c::new(p.I2C1, esp_hal::i2c::master::Config::default())
        .expect("sensor I2C config")
        .with_sda(p.GPIO17)
        .with_scl(p.GPIO18);

    // SD card slot of the T-Beam S3 Supreme on its own SPI bus. Cards start up at 400 kHz or less,
    // which is plenty for a log.
    #[cfg(feature = "balloon")]
    let sd_spi = {
        let mut spi = Spi::new(p.SPI3, SpiConfig::default().with_frequency(Rate::from_khz(400)).with_mode(Mode::_3))
            .expect("SD SPI config")
            .with_sck(p.GPIO36)
            .with_miso(p.GPIO37)
            .with_mosi(p.GPIO35);
        let cs = Output::new(p.GPIO47, Level::High, OutputConfig::default());
        // The IMU (QMI8658A) is on this same SPI bus (schematic: IMU_CS next to SD_CS). Keep it
        // deselected so it cannot drive MISO (without this the card did not answer in mode 0, and
        // answered with a stray bit before its reply).
        let imu_cs = Output::new(p.GPIO34, Level::High, OutputConfig::default());
        #[allow(clippy::mem_forget, reason = "a plain GPIO output; it must stay high for the whole run")]
        core::mem::forget(imu_cs);
        // SPI mode 3, not 0: in mode 0 a stray low bit comes before the card's answer (7F 01
        // instead of FF 01) and the card library rejects the card. Mode 3 gives a clean FF 01.
        // A card needs at least 74 clock cycles with CS high after power-up before it takes
        // commands. The card library cannot do that through an SPI device (it holds CS low).
        let _ = spi.write(&[0xFF; 16]);
        ExclusiveDevice::new(spi, cs, Delay).expect("SD SPI device")
    };

    // Only RX is configured here; the logger writes to UART0 itself.
    #[cfg(feature = "ground")]
    let console_rx = Console(
        Uart::new(p.UART0, UartConfig::default().with_baudrate(115_200))
            .expect("console UART config")
            .with_rx(p.GPIO44)
            .split()
            .0,
    );

    BoardPeripherals {
        burn_pin,
        rng,
        radio,
        #[cfg(feature = "balloon")]
        gps_uart,
        #[cfg(feature = "balloon")]
        rtc,
        #[cfg(feature = "balloon")]
        sensor_i2c,
        #[cfg(feature = "balloon")]
        cpu_ctrl: p.CPU_CTRL,
        #[cfg(feature = "balloon")]
        second_core_int: p.FROM_CPU_INTR1,
        #[cfg(feature = "balloon")]
        sd_spi,
        #[cfg(feature = "ground")]
        console_rx,
    }
}
