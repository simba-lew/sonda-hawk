//! Balloon log: every log line goes to the serial console as before and also into a RAM buffer,
//! which `sd_log_task` writes to a new file on the SD card (`L0001.TXT`, `L0002.TXT`, ... one file
//! per boot), so the log of a flight can be read afterwards. The card library blocks (for seconds
//! when the card is missing), so this task runs alone on the second CPU core (see `main.rs`). A
//! missing or failed card then never affects the rest of the firmware: the task only logs a
//! warning and retries.

use core::cell::RefCell;
use core::fmt::Write;

use critical_section::Mutex;
use embassy_time::{Delay, Duration, Instant, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::{Block, BlockDevice, BlockIdx, Mode, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use esp_hal::gpio::Output;
use esp_hal::spi::master::Spi;
use esp_hal::Blocking;
use heapless::Deque;
use log::{info, warn, Level, LevelFilter, Log, Metadata, Record};

/// Log lines waiting for the SD card. A line that does not fit is dropped whole.
const BUFFER_BYTES: usize = 8192;
/// How often the buffer is written to the card, and how often the file is flushed (so a power
/// loss costs at most this much of the log).
const WRITE_PERIOD_MS: u64 = 500;
const FLUSH_PERIOD_MS: u64 = 2_000;
/// Wait after a failed card start before trying again.
const RETRY_S: u64 = 10;
const MAX_FILES: u16 = 9_999;

static BUFFER: Mutex<RefCell<Deque<u8, BUFFER_BYTES>>> = Mutex::new(RefCell::new(Deque::new()));

type SdSpi = ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, Delay>;

/// Writes into `BUFFER`; the whole line is dropped if it does not fit.
struct Line {
    bytes: heapless::Vec<u8, 256>,
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // An overlong line is cut, not dropped.
        let n = s.len().min(self.bytes.capacity() - self.bytes.len());
        let _ = self.bytes.extend_from_slice(&s.as_bytes()[..n]);
        Ok(())
    }
}

struct TeeLogger;

impl Log for TeeLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        esp_println::println!("{} - {}", record.level(), record.args());
        let mut line = Line { bytes: heapless::Vec::new() };
        let _ = write!(line, "[{:>9}] {} - {}", Instant::now().as_millis(), record.level(), record.args());
        let _ = line.bytes.push(b'\n');
        critical_section::with(|cs| {
            let mut buf = BUFFER.borrow_ref_mut(cs);
            if buf.capacity() - buf.len() >= line.bytes.len() {
                for b in line.bytes {
                    let _ = buf.push_back(b);
                }
            }
        });
    }

    fn flush(&self) {}
}

/// Installs the logger (console + SD buffer). Call once, first thing in `main`.
pub fn init() {
    // SAFETY: called once at start-up, before any other task runs.
    unsafe {
        let _ = log::set_logger_racy(&TeeLogger);
        log::set_max_level_racy(LevelFilter::Info);
    }
}

/// No real-time clock: files get a fixed date.
struct NoClock;

impl TimeSource for NoClock {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp { year_since_1970: 56, zero_indexed_month: 0, zero_indexed_day: 0, hours: 0, minutes: 0, seconds: 0 }
    }
}

/// Moves up to `out.len()` bytes from the buffer into `out`; returns how many.
fn drain(out: &mut [u8]) -> usize {
    critical_section::with(|cs| {
        let mut buf = BUFFER.borrow_ref_mut(cs);
        let mut n = 0;
        while n < out.len() {
            match buf.pop_front() {
                Some(b) => out[n] = b,
                None => break,
            }
            n += 1;
        }
        n
    })
}

/// Makes the next card access start the card again. (`device` is declared to return the time
/// source type in embedded-sdmmc 0.9, hence the closure's return value.)
fn mark_uninit(vm: &VolumeManager<SdCard<SdSpi, Delay>, NoClock>) {
    vm.device(|sd| {
        sd.mark_card_uninit();
        NoClock
    });
}

#[derive(Debug)]
#[allow(dead_code)] // only read through `Debug`, in the warning
enum SdError {
    /// Which step failed, and the library's error.
    Card(&'static str, heapless::String<80>),
    NoFreeName,
}

fn card_error(step: &'static str, e: impl core::fmt::Debug) -> SdError {
    let mut text = heapless::String::new();
    let _ = write!(text, "{:?}", e);
    SdError::Card(step, text)
}

/// Checks the card's first partition before the library parses it: embedded-sdmmc 0.9 panics
/// (divide by zero, which resets the chip) on a boot sector that is not FAT16/FAT32, e.g. a card
/// without a partition table or an exFAT card. Only the partition types the library accepts are
/// looked into; it reports the others as an error by itself.
fn check_fat(vm: &VolumeManager<SdCard<SdSpi, Delay>, NoClock>) -> Result<(), SdError> {
    let mut result = Ok(());
    vm.device(|sd| {
        let mut block = [Block::new()];
        let read = |idx: u32, block: &mut [Block; 1]| {
            sd.read(block, BlockIdx(idx)).map_err(|e| card_error("read", e))
        };
        result = (|| {
            read(0, &mut block)?;
            let mbr = &block[0].contents;
            // partition 1: type at +4, first sector at +8
            if mbr[510] != 0x55 || mbr[511] != 0xAA || !matches!(mbr[446 + 4], 0x04 | 0x06 | 0x0B | 0x0C | 0x0E) {
                return Ok(());
            }
            let start = u32::from_le_bytes([mbr[454], mbr[455], mbr[456], mbr[457]]);
            read(start, &mut block)?;
            let bpb = &block[0].contents;
            let bytes_per_sector = u16::from_le_bytes([bpb[11], bpb[12]]);
            if bytes_per_sector != 512 || bpb[13] == 0 || bpb[16] == 0 {
                return Err(card_error("boot sector", "not FAT16/FAT32 (format it as FAT32 with an MBR)"));
            }
            Ok(())
        })();
        NoClock
    });
    result
}

/// Opens the next unused `Lnnnn.TXT` in the root directory of the first volume.
fn open_log(
    vm: &VolumeManager<SdCard<SdSpi, Delay>, NoClock>,
    name: &mut heapless::String<12>,
) -> Result<embedded_sdmmc::RawFile, SdError> {
    check_fat(vm)?;
    let volume = vm.open_raw_volume(VolumeIdx(0)).map_err(|e| card_error("open volume", e))?;
    let root = vm.open_root_dir(volume).map_err(|e| card_error("open root dir", e))?;
    for n in 1..=MAX_FILES {
        name.clear();
        let _ = write!(name, "L{:04}.TXT", n);
        // `ReadWriteCreate` fails if the file exists
        if let Ok(file) = vm.open_file_in_dir(root, name.as_str(), Mode::ReadWriteCreate) {
            return Ok(file);
        }
    }
    Err(SdError::NoFreeName)
}

/// Writes the log buffer to the SD card, for the whole run.
#[embassy_executor::task]
pub async fn sd_log_task(spi: SdSpi) {
    let vm = VolumeManager::new(SdCard::new(spi, Delay), NoClock);
    let mut chunk = [0u8; 256];
    loop {
        let mut name = heapless::String::<12>::new();
        let file = match open_log(&vm, &mut name) {
            Ok(f) => f,
            Err(e) => {
                warn!("SD log: card not usable ({:?}), retrying in {} s", e, RETRY_S);
                mark_uninit(&vm);
                Timer::after(Duration::from_secs(RETRY_S)).await;
                continue;
            }
        };
        info!("SD log: writing to {}", name);

        let mut last_flush = Instant::now();
        let failed = loop {
            Timer::after(Duration::from_millis(WRITE_PERIOD_MS)).await;
            let mut failed = false;
            loop {
                let n = drain(&mut chunk);
                if n == 0 {
                    break;
                }
                if vm.write(file, &chunk[..n]).is_err() {
                    failed = true;
                    break;
                }
            }
            if !failed && last_flush.elapsed() >= Duration::from_millis(FLUSH_PERIOD_MS) {
                last_flush = Instant::now();
                failed = vm.flush_file(file).is_err();
            }
            if failed {
                break true;
            }
        };
        if failed {
            warn!("SD log: write failed, reopening the card in {} s", RETRY_S);
            let _ = vm.close_file(file);
            mark_uninit(&vm);
            Timer::after(Duration::from_secs(RETRY_S)).await;
        }
    }
}
