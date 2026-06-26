//! Sensirion SHT45 temperature + humidity sensor (Adafruit #6174).
//!
//! Thin wrapper over the `sht4x` driver crate; we keep our own [`Sample`] type so the
//! rest of the firmware doesn't depend on the driver's representation (fixed-point).

use embedded_hal::i2c::I2c;
use esp_idf_svc::hal::delay::Delay;
use sht4x::{Precision, Sht4x};

/// A single temperature + humidity reading.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub temperature_c: f32,
    pub temperature_f: f32,
    pub humidity_pct: f32,
}

/// Construct the driver on a (shared) I2C bus at the default address 0x44.
pub fn new<I: I2c>(i2c: I) -> Sht4x<I, Delay> {
    Sht4x::new(i2c)
}

/// Trigger a high-precision measurement and convert it to f32 SI units.
pub fn read<I: I2c>(sensor: &mut Sht4x<I, Delay>, delay: &mut Delay) -> anyhow::Result<Sample> {
    let m = sensor
        .measure(Precision::High, delay)
        .map_err(|e| anyhow::anyhow!("SHT45 measure failed: {e:?}"))?;
    let temperature_c = m.temperature_celsius().to_num::<f32>();
    Ok(Sample {
        temperature_c,
        temperature_f: temperature_c * 9.0 / 5.0 + 32.0,
        humidity_pct: m.humidity_percent().to_num::<f32>(),
    })
}
