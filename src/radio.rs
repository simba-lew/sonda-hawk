//! SX1262 radio through the `lora-phy` crate (async: waits on the DIO1 interrupt pin instead
//! of polling). 869.525 MHz, SF7, BW125, CR4/5, private sync word, 8 symbol preamble,
//! explicit header, CRC on.
//!
//! Note: `lora-phy` does not discard packets whose radio-level CRC failed, it only logs it.

use embassy_time::{with_timeout, Delay, Duration, Instant, Timer};
use embedded_hal_async::spi::SpiDevice;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::gpio::{Input, Output};
use esp_hal::spi::master::Spi;
use esp_hal::Async;
use lora_phy::iv::GenericSx126xInterfaceVariant;
use lora_phy::mod_params::{Bandwidth, CodingRate, ModulationParams, PacketParams, RadioError, SpreadingFactor};
use lora_phy::sx126x::{self, Sx126x, Sx1262, TcxoCtrlVoltage};
use lora_phy::{LoRa, RxMode};

// Radio settings: must be identical on the balloon and the ground station.

/// 869.525 MHz is the 10% duty-cycle sub-band in the EU.
pub const RADIO_FREQ_HZ: u32 = 869_525_000;
pub const RADIO_TX_POWER_DBM: i32 = 10;
pub const RADIO_SPREADING_FACTOR: SpreadingFactor = SpreadingFactor::_7;
pub const RADIO_BANDWIDTH: Bandwidth = Bandwidth::_125KHz;

/// Pause after a radio error before trying again.
pub const RETRY_DELAY_MS: u64 = 100;
/// A transmission that takes longer than this is aborted.
const TX_TIMEOUT_MS: u64 = 1_500;

/// Largest packet the radio receives; also the size of the receive buffers.
pub const PAYLOAD_MAX: usize = 64;
const PREAMBLE_SYMBOLS: u16 = 8;

/// Longest wait the chip does by itself (the SX126x counts at most 248 symbols).
const MAX_RX_WINDOW_SYMBOLS: u16 = 248;
/// How long a receive may overrun its time before it is aborted.
const RX_BACKSTOP_MS: u64 = 600;

/// Do not start a listen with less than this left in the period.
pub const LISTEN_MIN_SLICE_MS: u64 = 100;

type SpiDev = ExclusiveDevice<Spi<'static, Async>, Output<'static>, Delay>;
type Iv = GenericSx126xInterfaceVariant<Output<'static>, Input<'static>>;

#[derive(Debug)]
pub enum LoraRadioError {
    /// The SX1262 did not answer a GetStatus command: wrong pins, no power, or no radio module.
    /// `status` 0xFF / 0x00 = nothing drives MISO; `busy_high` stuck high = BUSY wrong or chip held in reset.
    NoChip { status: u8, busy_high: bool },
    Driver(RadioError),
    TxTimeout,
}

impl From<RadioError> for LoraRadioError {
    fn from(e: RadioError) -> Self {
        LoraRadioError::Driver(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReceiveResult {
    Timeout,
    Packet { len: usize, rssi_dbm: i16, snr_db: f32 },
}

pub struct LoraRadio {
    lora: LoRa<Sx126x<SpiDev, Iv, Sx1262>, Delay>,
    modulation: ModulationParams,
    tx_params: PacketParams,
    rx_params: PacketParams,
}

impl LoraRadio {
    /// Resets and probes the chip, then sets it up. DIO2 switches the antenna and DIO3 powers the
    /// TCXO (both done by the chip); `rxen` is the receive-side antenna switch pin for boards that
    /// have one, `None` for boards where DIO2 does everything.
    pub async fn new(
        spi: Spi<'static, Async>,
        nss: Output<'static>,
        mut reset: Output<'static>,
        busy: Input<'static>,
        dio1: Input<'static>,
        rxen: Option<Output<'static>>,
    ) -> Result<Self, LoraRadioError> {
        reset.set_low();
        Timer::after(Duration::from_millis(10)).await;
        reset.set_high();
        Timer::after(Duration::from_millis(20)).await;
        let busy_high = busy.is_high();
        let mut device = ExclusiveDevice::new(spi, nss, Delay).map_err(|_| RadioError::SPI)?;
        // GetStatus (0xC0): a live chip answers with a non-zero status byte (chip mode in bits 6..4).
        // `lora-phy` panics (index out of bounds) instead of failing when nothing answers.
        let mut probe = [0xC0u8, 0x00];
        device.transfer_in_place(&mut probe).await.map_err(|_| RadioError::SPI)?;
        let status = probe[1];
        if busy_high || matches!(status, 0x00 | 0xFF) {
            return Err(LoraRadioError::NoChip { status, busy_high });
        }
        let iv = GenericSx126xInterfaceVariant::new(reset, dio1, busy, rxen, None)?;
        let config = sx126x::Config {
            chip: Sx1262,
            tcxo_ctrl: Some(TcxoCtrlVoltage::Ctrl1V8),
            use_dcdc: true,
            rx_boost: false,
        };
        // `false` = private network: LoRa sync word 0x1424
        let mut lora = LoRa::new(Sx126x::new(device, iv, config), false, Delay).await?;
        let modulation = lora.create_modulation_params(
            RADIO_SPREADING_FACTOR,
            RADIO_BANDWIDTH,
            CodingRate::_4_5,
            RADIO_FREQ_HZ,
        )?;
        let tx_params = lora.create_tx_packet_params(PREAMBLE_SYMBOLS, false, true, false, &modulation)?;
        let rx_params =
            lora.create_rx_packet_params(PREAMBLE_SYMBOLS, false, PAYLOAD_MAX as u8, true, false, &modulation)?;
        Ok(Self { lora, modulation, tx_params, rx_params })
    }

    /// Transmits `data`; gives up after `TX_TIMEOUT_MS` (every send timing out = wrong DIO1 pin).
    pub async fn send(&mut self, data: &[u8]) -> Result<(), LoraRadioError> {
        self.lora
            .prepare_for_tx(&self.modulation, &mut self.tx_params, RADIO_TX_POWER_DBM, data)
            .await?;
        match with_timeout(Duration::from_millis(TX_TIMEOUT_MS), self.lora.tx()).await {
            Ok(r) => Ok(r?),
            Err(_) => {
                let _ = self.lora.enter_standby().await;
                Err(LoraRadioError::TxTimeout)
            }
        }
    }

    /// Listen for up to `timeout_ms`. The payload is copied into `buf` (cut to its length).
    ///
    /// The wait is done by the radio chip, in windows of at most `MAX_RX_WINDOW_SYMBOLS` symbols
    /// that end only if no preamble was heard: a packet that has started is always received to
    /// the end, also when it begins just before `timeout_ms` is over (it then arrives a little
    /// late). Cutting the wait from the outside, as before, aborted such a packet.
    pub async fn receive(&mut self, buf: &mut [u8], timeout_ms: u32) -> Result<ReceiveResult, LoraRadioError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        let Self { lora, modulation, rx_params, .. } = self;
        let attempt = async {
            loop {
                let left_us = deadline.saturating_duration_since(Instant::now()).as_micros();
                let symbols = (left_us / symbol_us()).min(MAX_RX_WINDOW_SYMBOLS as u64) as u16;
                if symbols == 0 {
                    return Ok(ReceiveResult::Timeout);
                }
                lora.prepare_for_rx(RxMode::Single(symbols), modulation, rx_params).await?;
                match lora.rx(rx_params, buf).await {
                    Ok((len, status)) => {
                        return Ok(ReceiveResult::Packet {
                            len: len as usize,
                            rssi_dbm: status.rssi,
                            snr_db: status.snr as f32,
                        })
                    }
                    Err(RadioError::ReceiveTimeout) => continue, // the next window, if time is left
                    Err(e) => return Err(LoraRadioError::from(e)),
                }
            }
        };
        // Backstop: a damaged packet header ends the radio's wait without a signal to the driver.
        match with_timeout(Duration::from_millis(timeout_ms as u64 + RX_BACKSTOP_MS), attempt).await {
            Ok(r) => r,
            Err(_) => {
                self.lora.enter_standby().await?;
                Ok(ReceiveResult::Timeout)
            }
        }
    }
}

/// Duration of one LoRa symbol in microseconds: 2^SF / bandwidth.
fn symbol_us() -> u64 {
    (1u64 << RADIO_SPREADING_FACTOR.factor()) * 1_000_000 / RADIO_BANDWIDTH.hz() as u64
}
