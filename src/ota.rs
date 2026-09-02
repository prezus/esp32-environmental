//! ESP-IDF A/B rollback boundary. Download/install remains disabled until a signed
//! image and a physical Feather can exercise the complete AWS Jobs transition.

use std::ffi::CString;

use anyhow::{anyhow, Context};
use esp_idf_svc::sys::{
    esp_crt_bundle_attach, esp_http_client_config_t, esp_https_ota, esp_https_ota_config_t,
    esp_ota_mark_app_invalid_rollback_and_reboot, esp_ota_mark_app_valid_cancel_rollback, EspError,
    ESP_OK,
};

pub fn confirm_running_image() -> anyhow::Result<()> {
    check(
        unsafe { esp_ota_mark_app_valid_cancel_rollback() },
        "failed to mark running image valid",
    )
}

/// Download a signed image into the inactive OTA slot and select it for the
/// next boot. Signature verification is enforced by sdkconfig against the
/// public key carried by the currently running signed image.
pub fn install_signed_image(image_url: &str) -> anyhow::Result<()> {
    let url = CString::new(image_url).context("OTA URL contains NUL")?;
    let http_config = esp_http_client_config_t {
        url: url.as_ptr(),
        timeout_ms: 60_000,
        crt_bundle_attach: Some(esp_crt_bundle_attach),
        keep_alive_enable: true,
        ..Default::default()
    };
    let ota_config = esp_https_ota_config_t {
        http_config: &http_config,
        ..Default::default()
    };
    check(
        unsafe { esp_https_ota(&ota_config) },
        "signed HTTPS OTA installation failed",
    )
}

#[allow(dead_code)]
pub fn reject_running_image_and_reboot() -> anyhow::Result<()> {
    check(
        unsafe { esp_ota_mark_app_invalid_rollback_and_reboot() },
        "failed to roll back unhealthy image",
    )
}

fn check(code: i32, context: &'static str) -> anyhow::Result<()> {
    if code == ESP_OK {
        return Ok(());
    }
    let error = EspError::from(code)
        .map(|error| anyhow!(error))
        .unwrap_or_else(|| anyhow!("ESP-IDF error {code}"));
    Err(error).context(context)
}
