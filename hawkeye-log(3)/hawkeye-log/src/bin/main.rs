#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use hawkeye::board;
use log::error;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    // Restart the chip: all GPIOs go back to their reset state, so a burn in progress ends.
    esp_hal::system::software_reset()
}

esp_bootloader_esp_idf::esp_app_desc!();

#[cfg(feature = "balloon")]
static SECOND_CORE_STACK: static_cell::ConstStaticCell<esp_hal::system::Stack<8192>> =
    static_cell::ConstStaticCell::new(esp_hal::system::Stack::new());
#[cfg(feature = "balloon")]
static SD_EXECUTOR: static_cell::StaticCell<esp_rtos::embassy::Executor> = static_cell::StaticCell::new();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // The balloon also keeps its log on the SD card.
    #[cfg(feature = "balloon")]
    hawkeye::logfile::init();
    #[cfg(feature = "ground")]
    esp_println::logger::init_logger_from_env();

    let hw = board::init().await;
    let radio = match hw.radio {
        Ok(r) => r,
        Err(e) => panic!("radio init failed: {:?} (check wiring, board power, and the pins)", e),
    };

    #[cfg(feature = "ground")]
    {
        let _ = spawner;
        hawkeye::ground::ground_task(radio, hw.console_rx, hw.rng).await;
    }
    #[cfg(feature = "balloon")]
    {
        // The SD card library blocks (a missing card for seconds), so the card lives alone on the
        // second core: it must never hold up the burn, the radio or the failsafe on this core.
        let sd_spi = hw.sd_spi;
        esp_rtos::start_second_core(hw.cpu_ctrl, hw.second_core_int, SECOND_CORE_STACK.take(), move || {
            SD_EXECUTOR.init(esp_rtos::embassy::Executor::new()).run(|spawner| {
                spawner.spawn(hawkeye::logfile::sd_log_task(sd_spi).expect("spawn sd log"));
            })
        });
        spawner.spawn(hawkeye::balloon::burn_task(hw.burn_pin, hw.rtc).expect("spawn burn"));
        spawner.spawn(hawkeye::sensors::gps::gps_task(hw.gps_uart).expect("spawn gps"));
        spawner.spawn(hawkeye::sensors::env::env_task(hw.sensor_i2c).expect("spawn env"));
        hawkeye::balloon::balloon_task(radio, hw.rng).await
    }
}
