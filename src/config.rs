//! Compile-time configuration, populated from `cfg.toml` by the `toml-cfg` crate.
//!
//! The table header in `cfg.toml` must be `[esp32-environmental]` (the crate name).

#[toml_cfg::toml_config]
pub struct Config {
    #[default("")]
    pub wifi_ssid: &'static str,
    #[default("")]
    pub wifi_psk: &'static str,
    #[default(30)]
    pub sample_interval_secs: u64,
    #[default("esp32-env")]
    pub hostname: &'static str,
    #[default(false)]
    pub aws_iot_enabled: bool,
    #[default("/sdcard/aws-iot.json")]
    pub aws_iot_config_path: &'static str,
}
