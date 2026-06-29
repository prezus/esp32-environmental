//! ESP32-S3 environmental logger.
//!
//! Reads temperature + humidity from an SHT45 (I2C), timestamps each sample from a
//! PCF8523 RTC (I2C), appends it to a daily CSV on the SD card (SPI/FAT), and serves
//! a dashboard + CSV export over WiFi, with a web-configurable temperature offset.
//!
//! ── Pin map (Adafruit ESP32-S3 Feather #5477 + Adalogger FeatherWing #2922) ──
//! VERIFY these against your board silkscreen / the Adalogger's CS solder jumper.

mod config;
mod rtc;
mod sensor;
mod server;
mod storage;

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use embedded_hal::i2c::I2c;
use embedded_hal_bus::i2c::RefCellDevice;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::fs::fatfs::Fatfs;
use esp_idf_svc::hal::delay::{Delay, FreeRtos};
use esp_idf_svc::hal::gpio::{AnyIOPin, PinDriver};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::hal::sd::{spi::SdSpiHostDriver, SdCardConfiguration, SdCardDriver};
use esp_idf_svc::hal::spi::{config::DriverConfig, Dma, SpiDriver};
use esp_idf_svc::io::vfs::MountedFatfs;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use esp_idf_svc::sntp::EspSntp;
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration as WifiConfiguration, EspWifi,
};
use nxp_pcf8523::driver::Pcf8523;
use nxp_pcf8523::typedefs::Variant;
use sht4x::Sht4x;
use time::OffsetDateTime;

use config::CONFIG;
use server::Latest;

/// Shared "latest reading", read by the `/api/latest` handler.
pub type SharedLatest = Arc<Mutex<Option<Latest>>>;
/// Serializes all SD/FAT access (the sampling writer vs. the HTTP readers).
pub type SdGuard = Arc<Mutex<()>>;
/// User temperature-calibration offset in °F, shared between the web UI and sampling.
pub type Calibration = Arc<Mutex<f32>>;

fn main() -> anyhow::Result<()> {
    // Required boilerplate for esp-idf-sys / runtime linking + logging.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // Localize displayed timestamps to US Mountain Time (handles MST/MDT automatically).
    set_timezone("MST7MDT,M3.2.0,M11.1.0");

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;

    // ── I2C power rail ──────────────────────────────────────────────────────
    // On the S3 Feather the STEMMA QT / I2C rail is gated by GPIO7 (I2C_POWER);
    // drive it high and keep it driven for the lifetime of the program.
    let mut i2c_power = PinDriver::output(pins.gpio7)?;
    i2c_power.set_high()?;
    let _i2c_power = i2c_power; // keep alive so the pin stays high

    // ── I2C bus (SHT45 @ 0x44, PCF8523 @ 0x68) ─────────────────────────────
    // One bus, two devices: wrap the driver in a RefCell and hand each device a
    // RefCellDevice. Everything I2C happens on this (the main) thread, so the
    // RefCell is only ever borrowed by one call at a time.
    let i2c_cfg = I2cConfig::new().baudrate(400.kHz().into());
    let mut i2c = I2cDriver::new(peripherals.i2c0, pins.gpio3, pins.gpio4, &i2c_cfg)?;
    scan_i2c(&mut i2c); // diagnostic: expect 0x44 (SHT45) and 0x68 (PCF8523)
    let i2c_ref = RefCell::new(i2c);
    let mut delay = Delay::new_default();
    let mut sht = sensor::new(RefCellDevice::new(&i2c_ref));
    let mut pcf = rtc::new(RefCellDevice::new(&i2c_ref))?;

    // ── SD card over SPI, mounted as FAT at /sdcard ────────────────────────
    let spi_driver = SpiDriver::new(
        peripherals.spi2,
        pins.gpio36,       // SCK
        pins.gpio35,       // MOSI (SDO)
        Some(pins.gpio37), // MISO (SDI)
        &DriverConfig::default().dma(Dma::Auto(4096)),
    )?;
    let sd_card = SdCardDriver::new_spi(
        SdSpiHostDriver::new(
            &spi_driver,
            Some(pins.gpio10), // SD chip-select = Adalogger default CS (D10); see README if jumper cut
            AnyIOPin::none(),  // card detect (unused)
            AnyIOPin::none(),  // write protect (unused)
            AnyIOPin::none(),  // interrupt (unused)
            None,              // wp_active_high (ESP-IDF >= 5.2; no WP pin -> None)
        )?,
        &SdCardConfiguration::new(),
    )?;
    // Keep `_fs` alive for the whole run, otherwise the card unmounts.
    let _fs = MountedFatfs::mount(Fatfs::new_sdcard(0, sd_card)?, storage::MOUNT_POINT, 4)?;
    storage::init()?;
    log::info!("SD card mounted at {}", storage::MOUNT_POINT);

    // ── WiFi (STA) + SNTP -> RTC + mDNS ────────────────────────────────────
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // Persistent temperature-calibration offset (°F), stored in NVS as centi-°F so it
    // survives reboots and SD-card reformats. Editable from the web UI.
    let cal_nvs = EspNvs::new(nvs.clone(), "cal", true)?;
    let calibration: Calibration = Arc::new(Mutex::new(
        cal_nvs.get_i32("temp_off_cf")?.unwrap_or(0) as f32 / 100.0,
    ));
    log::info!(
        "temperature offset: {:+.1} °F",
        *calibration.lock().unwrap()
    );

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;
    // These handles must outlive the program: dropping them tears down WiFi / SNTP.
    let mut _wifi = None;
    let mut _sntp = None;
    match connect_wifi(&mut wifi) {
        Ok(()) => {
            let sntp = EspSntp::new_default()?;
            if wait_for_time(20_000) {
                set_rtc_from_system(&mut pcf);
            } else {
                log::warn!("SNTP did not sync within 20s; RTC left as-is");
            }
            _sntp = Some(sntp);
            _wifi = Some(wifi);
        }
        Err(e) => {
            // Offline is fine: the RTC backup battery keeps time, so we keep logging.
            log::warn!("WiFi unavailable ({e}); continuing offline (RTC timestamps only)");
        }
    }

    // ── HTTP server ────────────────────────────────────────────────────────
    let latest: SharedLatest = Arc::new(Mutex::new(None));
    let sd_guard: SdGuard = Arc::new(Mutex::new(()));
    let _http = server::start(latest.clone(), sd_guard.clone(), calibration.clone())?;

    // ── Sampling loop ──────────────────────────────────────────────────────
    let interval_ms = (CONFIG.sample_interval_secs.max(1) * 1000) as u32;
    log::info!("logging every {}s", CONFIG.sample_interval_secs);
    let mut last_persisted = *calibration.lock().unwrap();
    loop {
        match sample(&mut sht, &mut pcf, &mut delay) {
            Ok((ts, raw)) => {
                let s = apply_calibration(raw, *calibration.lock().unwrap());
                *latest.lock().unwrap() = Some(Latest {
                    iso8601: ts.iso8601.clone(),
                    temperature_c: s.temperature_c,
                    temperature_f: s.temperature_f,
                    humidity_pct: s.humidity_pct,
                });
                {
                    let _g = sd_guard.lock().unwrap();
                    if let Err(e) = storage::append(&ts, &s) {
                        log::error!("CSV append failed: {e}");
                    }
                }
                log::info!(
                    "{} | {:.2} °C / {:.2} °F | {:.2} %RH",
                    ts.iso8601,
                    s.temperature_c,
                    s.temperature_f,
                    s.humidity_pct
                );
            }
            Err(e) => log::warn!("skipped sample: {e}"),
        }

        // Persist the calibration offset to NVS if it was changed via the web UI.
        let current = *calibration.lock().unwrap();
        if (current - last_persisted).abs() > f32::EPSILON {
            match cal_nvs.set_i32("temp_off_cf", (current * 100.0).round() as i32) {
                Ok(()) => {
                    last_persisted = current;
                    log::info!("calibration saved to NVS: {current:+.1} °F");
                }
                Err(e) => log::error!("failed to save calibration: {e}"),
            }
        }

        FreeRtos::delay_ms(interval_ms);
    }
}

/// Apply the user's temperature calibration offset (a delta in °F) to a raw sample.
fn apply_calibration(s: sensor::Sample, offset_f: f32) -> sensor::Sample {
    let temperature_c = s.temperature_c + offset_f * 5.0 / 9.0;
    sensor::Sample {
        temperature_c,
        temperature_f: temperature_c * 9.0 / 5.0 + 32.0,
        humidity_pct: s.humidity_pct,
    }
}

/// Read the sensor and stamp it with the current RTC time.
fn sample<I: I2c, V: Variant>(
    sht: &mut Sht4x<I, Delay>,
    pcf: &mut Pcf8523<I, V>,
    delay: &mut Delay,
) -> anyhow::Result<(rtc::Timestamp, sensor::Sample)> {
    let s = sensor::read(sht, delay)?;
    let ts = rtc::now(pcf)?;
    Ok((ts, s))
}

fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'static>>) -> anyhow::Result<()> {
    if CONFIG.wifi_ssid.is_empty() {
        return Err(anyhow!("wifi_ssid not set in cfg.toml"));
    }
    wifi.set_configuration(&WifiConfiguration::Client(ClientConfiguration {
        ssid: CONFIG
            .wifi_ssid
            .try_into()
            .map_err(|_| anyhow!("SSID too long"))?,
        password: CONFIG
            .wifi_psk
            .try_into()
            .map_err(|_| anyhow!("password too long"))?,
        auth_method: if CONFIG.wifi_psk.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        ..Default::default()
    }))?;
    wifi.start()?;

    // Retry the connect/DHCP step a few times — the signal here is marginal, so a
    // single attempt often times out (ESP_ERR_TIMEOUT) even when it would succeed.
    const ATTEMPTS: u32 = 6;
    let mut last_err = None;
    for attempt in 1..=ATTEMPTS {
        match wifi.connect().and_then(|_| wifi.wait_netif_up()) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                log::warn!("WiFi connect attempt {attempt}/{ATTEMPTS} failed: {e}");
                last_err = Some(e);
                let _ = wifi.disconnect();
                FreeRtos::delay_ms(2000);
            }
        }
    }
    if let Some(e) = last_err {
        return Err(e.into());
    }

    let ip = wifi.wifi().sta_netif().get_ip_info()?;
    log::info!("WiFi connected: dashboard at http://{}/", ip.ip);
    Ok(())
}

/// Current system clock as Unix epoch seconds, if readable.
fn system_unix() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// Wait until the system clock has been set to a plausibly-real time (after late 2023),
/// which is how we detect that SNTP has actually synced. Returns false on timeout.
fn wait_for_time(timeout_ms: u32) -> bool {
    let mut waited = 0;
    loop {
        if matches!(system_unix(), Some(u) if u > 1_700_000_000) {
            return true;
        }
        if waited >= timeout_ms {
            return false;
        }
        FreeRtos::delay_ms(200);
        waited += 200;
    }
}

/// Push the (SNTP-synced) system time into the PCF8523 RTC, in UTC.
fn set_rtc_from_system<I: I2c, V: Variant>(pcf: &mut Pcf8523<I, V>) {
    match system_unix().and_then(|u| OffsetDateTime::from_unix_timestamp(u).ok()) {
        Some(dt) => match rtc::set(pcf, dt) {
            Ok(()) => log::info!("RTC set from SNTP (UTC {dt})"),
            Err(e) => log::error!("failed to set RTC: {e}"),
        },
        None => log::error!("no system time available to set RTC"),
    }
}

/// Set the process timezone (POSIX TZ string) so libc `localtime_r` localizes output.
fn set_timezone(tz: &str) {
    if let Ok(tz) = std::ffi::CString::new(tz) {
        unsafe {
            esp_idf_svc::sys::setenv(c"TZ".as_ptr(), tz.as_ptr(), 1);
            esp_idf_svc::sys::tzset();
        }
    }
}

/// Probe every 7-bit I2C address and log which ones acknowledge. Diagnostic only.
fn scan_i2c(i2c: &mut I2cDriver<'_>) {
    use esp_idf_svc::hal::delay::BLOCK;
    let mut found = Vec::new();
    for addr in 0x08u8..0x78 {
        if i2c.write(addr, &[], BLOCK).is_ok() {
            found.push(format!("0x{addr:02X}"));
        }
    }
    if found.is_empty() {
        log::warn!("I2C scan: no devices responded (check wiring / STEMMA QT cable)");
    } else {
        log::info!("I2C scan: found {}", found.join(", "));
    }
}
