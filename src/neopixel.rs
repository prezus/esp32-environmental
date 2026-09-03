use std::sync::{Arc, Mutex};

use anyhow::Context;
use environmental_core::NeoPixelColor;
use ws2812_esp32_rmt_driver::Ws2812Esp32RmtDriver;

pub type SharedNeoPixel = Arc<Mutex<Ws2812Esp32RmtDriver<'static>>>;

pub fn set_color(neo_pixel: &SharedNeoPixel, color: NeoPixelColor) -> anyhow::Result<()> {
    let bytes = match color {
        // The onboard WS2812 expects GRB channel order. Values are deliberately
        // capped to keep a status indicator from becoming uncomfortably bright.
        NeoPixelColor::Off => [0, 0, 0],
        NeoPixelColor::Red => [0, 32, 0],
        NeoPixelColor::Green => [32, 0, 0],
        NeoPixelColor::Blue => [0, 0, 32],
        NeoPixelColor::Amber => [12, 32, 0],
        NeoPixelColor::Purple => [0, 24, 24],
        NeoPixelColor::White => [16, 16, 16],
    };
    neo_pixel
        .lock()
        .map_err(|_| anyhow::anyhow!("NeoPixel mutex poisoned"))?
        .write_blocking(bytes.into_iter())
        .context("NeoPixel RMT write failed")
}
