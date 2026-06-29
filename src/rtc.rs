//! PCF8523 real-time clock on the Adalogger FeatherWing (Adafruit #2922).
//!
//! Thin wrapper over the `nxp-pcf8523` driver crate. The RTC is battery-backed
//! (CR1220), so it keeps time across power loss and timestamps work offline.

use embedded_hal::i2c::I2c;
use nxp_pcf8523::driver::Pcf8523;
use nxp_pcf8523::typedefs::{Pcf8523T, Variant};
use time::OffsetDateTime;

// PCF8523 time registers (auto-incrementing from seconds).
const REG_SECONDS: u8 = 0x03;
const REG_MINUTES: u8 = 0x04;
const REG_HOURS: u8 = 0x05;
const REG_DAYS: u8 = 0x06;
const REG_WEEKDAYS: u8 = 0x07;
const REG_MONTHS: u8 = 0x08;
const REG_YEARS: u8 = 0x09;

/// A timestamp resolved from the RTC, ready for logging.
#[derive(Clone, Debug)]
pub struct Timestamp {
    /// Seconds since the Unix epoch (UTC).
    pub unix: i64,
    /// ISO-8601 / RFC-3339 string, e.g. "2026-06-26T14:03:22Z".
    pub iso8601: String,
    /// Calendar date "YYYY-MM-DD" (used as the daily log filename).
    pub date: String,
}

/// Construct the driver on a (shared) I2C bus. The Adalogger uses the PCF8523T variant.
/// Pings the device, so this fails if the RTC isn't responding.
pub fn new<I: I2c>(i2c: I) -> anyhow::Result<Pcf8523<I, Pcf8523T>> {
    Pcf8523::new(i2c, Pcf8523T {}).map_err(|e| anyhow::anyhow!("PCF8523 not found: {e:?}"))
}

/// Read the current time and resolve it to a [`Timestamp`].
///
/// The RTC holds UTC; we convert to local time (per the `TZ` set at startup) for the
/// human-readable fields, while `unix` stays true UTC epoch seconds.
pub fn now<I: I2c, V: Variant>(rtc: &mut Pcf8523<I, V>) -> anyhow::Result<Timestamp> {
    if rtc
        .lost_power()
        .map_err(|e| anyhow::anyhow!("RTC read failed: {e:?}"))?
    {
        anyhow::bail!("RTC lost power; time invalid until SNTP sync");
    }
    let dt = rtc
        .now()
        .map_err(|e| anyhow::anyhow!("RTC read failed: {e:?}"))?;
    let unix = dt.timestamp() as i64;
    let (iso8601, date) = local_format(unix);
    Ok(Timestamp {
        unix,
        iso8601,
        date,
    })
}

/// Convert a UTC epoch into a local ISO-8601 string (with offset) and a local
/// "YYYY-MM-DD" date, using libc `localtime_r` (honours the process `TZ`).
fn local_format(unix: i64) -> (String, String) {
    use esp_idf_svc::sys::{localtime_r, time_t, tm};

    let t = unix as time_t;
    let mut out: tm = unsafe { core::mem::zeroed() };
    unsafe {
        localtime_r(&t, &mut out);
    }
    let year = out.tm_year + 1900;
    let mon = out.tm_mon + 1;
    let day = out.tm_mday;
    // For Mountain Time the offset is -07:00 (MST) or -06:00 (MDT when tm_isdst > 0).
    let offset = if out.tm_isdst > 0 { "-06:00" } else { "-07:00" };
    (
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}",
            year, mon, day, out.tm_hour, out.tm_min, out.tm_sec, offset
        ),
        format!("{:04}-{:02}-{:02}", year, mon, day),
    )
}

/// Write a UTC time into the RTC (used after an SNTP sync) and start the oscillator.
///
/// We deliberately avoid the crate's `set_datetime`: it batches the time registers as
/// six *consecutive* embedded-hal `Write` operations, and per the embedded-hal 1.0 spec
/// adjacent writes are concatenated with no repeated-start. The PCF8523 then auto-
/// increments and writes the register addresses as data, shifting (corrupting) the time.
/// Writing one register per transaction via `write_reg` avoids that.
pub fn set<I: I2c, V: Variant>(rtc: &mut Pcf8523<I, V>, dt: OffsetDateTime) -> anyhow::Result<()> {
    let bcd = |d: u8| ((d / 10) << 4) | (d % 10);
    let writes = [
        (REG_SECONDS, bcd(dt.second())),
        (REG_MINUTES, bcd(dt.minute())),
        (REG_HOURS, bcd(dt.hour())),
        (REG_DAYS, bcd(dt.day())),
        (REG_WEEKDAYS, dt.weekday().number_days_from_sunday()),
        (REG_MONTHS, bcd(dt.month() as u8)),
        (REG_YEARS, bcd((dt.year() - 2000) as u8)),
    ];
    for (reg, val) in writes {
        rtc.write_reg(reg, val)
            .map_err(|e| anyhow::anyhow!("RTC write reg {reg:#x} failed: {e:?}"))?;
    }
    rtc.start()
        .map_err(|e| anyhow::anyhow!("RTC start failed: {e:?}"))?;
    Ok(())
}
