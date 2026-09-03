use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceBundle {
    wifi_ssid: String,
    wifi_psk: String,
    aws_iot_endpoint: String,
    device_id: String,
    certificate_file: PathBuf,
    private_key_file: PathBuf,
}

fn main() {
    println!("cargo:rerun-if-changed=sdkconfig.defaults");
    println!("cargo:rerun-if-changed=partitions.csv");
    println!("cargo:rerun-if-changed=cfg.toml");
    println!("cargo:rerun-if-env-changed=MOTHERSHIP_DEVICE_BUNDLE");
    write_device_bundle().expect("failed to generate embedded device bundle");
    embuild::espidf::sysenv::output();
}

fn write_device_bundle() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(env::var("OUT_DIR")?).join("device_bundle.rs");
    let Some(bundle_path) = env::var_os("MOTHERSHIP_DEVICE_BUNDLE").map(PathBuf::from) else {
        return write_generated(&output, false, "", "", "", "", None, None);
    };
    println!("cargo:rerun-if-changed={}", bundle_path.display());
    let raw = fs::read(&bundle_path)?;
    let bundle: DeviceBundle = serde_json::from_slice(&raw)?;
    validate(&bundle)?;
    let certificate_path = resolve_relative(&bundle_path, &bundle.certificate_file);
    let private_key_path = resolve_relative(&bundle_path, &bundle.private_key_file);
    println!("cargo:rerun-if-changed={}", certificate_path.display());
    println!("cargo:rerun-if-changed={}", private_key_path.display());
    let certificate = fs::read_to_string(&certificate_path)?;
    let private_key = fs::read_to_string(&private_key_path)?;
    if !certificate.contains("-----BEGIN CERTIFICATE-----") {
        return Err("device certificate is not PEM".into());
    }
    if !private_key.contains("-----BEGIN PRIVATE KEY-----")
        && !private_key.contains("-----BEGIN RSA PRIVATE KEY-----")
        && !private_key.contains("-----BEGIN EC PRIVATE KEY-----")
    {
        return Err("device private key is not PEM".into());
    }
    write_generated(
        &output,
        true,
        &bundle.wifi_ssid,
        &bundle.wifi_psk,
        &bundle.aws_iot_endpoint,
        &bundle.device_id,
        Some(&certificate_path),
        Some(&private_key_path),
    )
}

fn resolve_relative(bundle_path: &Path, value: &Path) -> PathBuf {
    if value.is_absolute() {
        value.to_owned()
    } else {
        bundle_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(value)
    }
}

fn validate(bundle: &DeviceBundle) -> Result<(), Box<dyn std::error::Error>> {
    if bundle.wifi_ssid.is_empty() || bundle.wifi_ssid.len() > 32 {
        return Err("wifiSsid must contain 1-32 bytes".into());
    }
    if bundle.wifi_psk.len() < 8 || bundle.wifi_psk.len() > 63 {
        return Err("wifiPsk must contain 8-63 bytes".into());
    }
    if bundle.aws_iot_endpoint.is_empty()
        || bundle.aws_iot_endpoint.contains('/')
        || bundle.aws_iot_endpoint.contains(':')
    {
        return Err("awsIotEndpoint must be a hostname".into());
    }
    if bundle.device_id.is_empty() || bundle.device_id.bytes().any(|byte| byte == 0) {
        return Err("deviceId must be non-empty and contain no NUL".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_generated(
    output: &Path,
    enabled: bool,
    wifi_ssid: &str,
    wifi_psk: &str,
    endpoint: &str,
    device_id: &str,
    certificate_path: Option<&Path>,
    private_key_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let certificate = certificate_path.map_or_else(
        || "b\"\"".to_owned(),
        |path| format!("include_bytes!({:?})", path.display().to_string()),
    );
    let private_key = private_key_path.map_or_else(
        || "b\"\"".to_owned(),
        |path| format!("include_bytes!({:?})", path.display().to_string()),
    );
    fs::write(
        output,
        format!(
            "pub const ENABLED: bool = {enabled:?};\n\
             pub const WIFI_SSID: &str = {wifi_ssid:?};\n\
             pub const WIFI_PSK: &str = {wifi_psk:?};\n\
             pub const AWS_IOT_ENDPOINT: &str = {endpoint:?};\n\
             pub const DEVICE_ID: &str = {device_id:?};\n\
             pub const DEVICE_CERTIFICATE_PEM: &[u8] = {certificate};\n\
             pub const DEVICE_PRIVATE_KEY_PEM: &[u8] = {private_key};\n"
        ),
    )?;
    Ok(())
}
