use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TELEMETRY_SCHEMA_VERSION: u8 = 1;
pub const SHADOW_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentalData {
    pub temperature_c: f32,
    pub temperature_f: f32,
    pub humidity_percent: f32,
    pub calibration_offset_f: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TelemetryEnvelope {
    pub schema_version: u8,
    pub event_id: String,
    pub device_id: String,
    pub boot_id: String,
    pub sequence: u64,
    pub observed_at: String,
    pub time_quality: TimeQuality,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: EnvironmentalData,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeQuality {
    Synced,
    Rtc,
    Unknown,
}

impl TelemetryEnvelope {
    pub fn new(
        event_id: String,
        device_id: String,
        boot_id: String,
        sequence: u64,
        observed_at: String,
        time_quality: TimeQuality,
        data: EnvironmentalData,
    ) -> Self {
        Self {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            event_id,
            device_id,
            boot_id,
            sequence,
            observed_at,
            time_quality,
            event_type: "environment.observation".into(),
            data,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationAck {
    pub accepted: bool,
    pub duplicate: bool,
    pub event_id: String,
}

pub fn parse_application_ack(payload: &[u8]) -> Result<ApplicationAck, String> {
    let ack: ApplicationAck = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
    if !ack.accepted || !is_identifier(&ack.event_id) {
        return Err("invalid application acknowledgement".into());
    }
    Ok(ack)
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NeoPixelColor {
    #[default]
    Off,
    Red,
    Green,
    Blue,
    Amber,
    Purple,
    White,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesiredConfiguration {
    pub sample_interval_seconds: u64,
    pub temperature_offset_f: f32,
    pub neo_pixel_color: NeoPixelColor,
}

impl Default for DesiredConfiguration {
    fn default() -> Self {
        Self {
            sample_interval_seconds: 30,
            temperature_offset_f: 0.0,
            neo_pixel_color: NeoPixelColor::Off,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShadowDelta {
    pub version: u64,
    pub configuration: DesiredConfiguration,
    pub unsupported_keys: Vec<String>,
}

pub fn parse_shadow_document(payload: &[u8]) -> Result<Option<ShadowDelta>, String> {
    let root: Value = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
    let version = root
        .get("version")
        .and_then(Value::as_u64)
        .ok_or("missing Shadow version")?;
    let state = root
        .get("state")
        .and_then(Value::as_object)
        .ok_or("missing Shadow state")?;
    let desired = if state.contains_key("desired") {
        match state.get("desired").and_then(Value::as_object) {
            Some(desired) => desired,
            None => return Ok(None),
        }
    } else {
        state
    };
    if desired.is_empty() {
        return Ok(None);
    }
    let sample_interval_seconds = desired
        .get("sampleIntervalSeconds")
        .and_then(Value::as_u64)
        .filter(|value| (10..=3_600).contains(value))
        .ok_or("invalid sampleIntervalSeconds")?;
    let temperature_offset_f = desired
        .get("temperatureOffsetF")
        .and_then(Value::as_f64)
        .filter(|value| (-20.0..=20.0).contains(value))
        .ok_or("invalid temperatureOffsetF")? as f32;
    let neo_pixel_color = desired
        .get("neoPixelColor")
        .map(|value| serde_json::from_value(value.clone()).map_err(|_| "invalid neoPixelColor"))
        .transpose()?
        .unwrap_or_default();
    let unsupported_keys = desired
        .keys()
        .filter(|key| {
            key.as_str() != "sampleIntervalSeconds"
                && key.as_str() != "temperatureOffsetF"
                && key.as_str() != "neoPixelColor"
        })
        .cloned()
        .collect();
    Ok(Some(ShadowDelta {
        version,
        configuration: DesiredConfiguration {
            sample_interval_seconds,
            temperature_offset_f,
            neo_pixel_color,
        },
        unsupported_keys,
    }))
}

pub fn reported_shadow(configuration: &DesiredConfiguration, desired_version: u64) -> Value {
    serde_json::json!({"state":{"reported":{
        "schemaVersion": SHADOW_SCHEMA_VERSION,
        "desiredVersionApplied": desired_version,
        "sampleIntervalSeconds": configuration.sample_interval_seconds,
        "temperatureOffsetF": configuration.temperature_offset_f,
        "neoPixelColor": configuration.neo_pixel_color
    }}})
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OtaJobDocument {
    pub operation: String,
    pub firmware_version: String,
    pub image_url: String,
    pub expires_at_unix: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OtaJob {
    pub job_id: String,
    pub execution_version: i64,
    pub document: OtaJobDocument,
}

pub fn parse_next_job(
    payload: &[u8],
    now_unix: i64,
    current_version: &str,
) -> Result<Option<OtaJob>, String> {
    let root: Value = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
    let Some(execution) = root.get("execution") else {
        return Ok(None);
    };
    let job_id = execution
        .get("jobId")
        .and_then(Value::as_str)
        .filter(|value| is_identifier(value))
        .ok_or("invalid jobId")?;
    let execution_version = execution
        .get("versionNumber")
        .and_then(Value::as_i64)
        .ok_or("missing execution version")?;
    let document: OtaJobDocument = serde_json::from_value(
        execution
            .get("jobDocument")
            .cloned()
            .ok_or("missing jobDocument")?,
    )
    .map_err(|error| error.to_string())?;
    if document.operation != "install"
        || document.firmware_version.is_empty()
        || document.firmware_version == current_version
        || document.expires_at_unix <= now_unix
        || !document.image_url.starts_with("https://")
        || document.image_url.len() > 2_048
    {
        return Err("unsafe or inapplicable OTA job".into());
    }
    Ok(Some(OtaJob {
        job_id: job_id.into(),
        execution_version,
        document,
    }))
}

pub fn job_status(status: &str, expected_version: i64, detail: &str) -> Value {
    serde_json::json!({"status":status,"expectedVersion":expected_version,"statusDetails":{"detail":detail}})
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_contract_matches_cloud_shape() {
        let envelope = TelemetryEnvelope::new(
            "device-1:42".into(),
            "device-1".into(),
            "boot-1".into(),
            42,
            "2026-09-02T12:00:00.000Z".into(),
            TimeQuality::Rtc,
            EnvironmentalData {
                temperature_c: 22.4,
                temperature_f: 72.32,
                humidity_percent: 45.1,
                calibration_offset_f: 0.0,
            },
        );
        let value = serde_json::to_value(envelope).unwrap();
        assert_eq!(value["type"], "environment.observation");
        assert!((value["data"]["humidityPercent"].as_f64().unwrap() - 45.1).abs() < 0.001);
    }

    #[test]
    fn application_ack_requires_acceptance_and_safe_identity() {
        assert!(parse_application_ack(
            br#"{"accepted":true,"duplicate":true,"eventId":"device-1:42"}"#
        )
        .is_ok());
        assert!(parse_application_ack(
            br#"{"accepted":false,"duplicate":false,"eventId":"device-1:42"}"#
        )
        .is_err());
        assert!(parse_application_ack(
            br#"{"accepted":true,"duplicate":false,"eventId":"../bad"}"#
        )
        .is_err());
    }

    #[test]
    fn shadow_delta_and_get_response_share_one_parser() {
        let delta = parse_shadow_document(
            br#"{"version":7,"state":{"sampleIntervalSeconds":60,"temperatureOffsetF":-2.5,"neoPixelColor":"purple"}}"#,
        )
        .unwrap()
        .unwrap();
        let get = parse_shadow_document(br#"{"version":7,"state":{"desired":{"sampleIntervalSeconds":60,"temperatureOffsetF":-2.5,"neoPixelColor":"purple"}}}"#)
            .unwrap().unwrap();
        assert_eq!(delta, get);
        assert_eq!(delta.configuration.neo_pixel_color, NeoPixelColor::Purple);
        assert_eq!(
            reported_shadow(&delta.configuration, 7)["state"]["reported"]["desiredVersionApplied"],
            7
        );
    }

    #[test]
    fn jobs_require_https_future_expiry_and_a_new_version() {
        let payload = br#"{"execution":{"jobId":"ota-1","versionNumber":3,"jobDocument":{"operation":"install","firmwareVersion":"0.2.0","imageUrl":"https://example.invalid/signed.bin","expiresAtUnix":200}}}"#;
        let job = parse_next_job(payload, 100, "0.1.0").unwrap().unwrap();
        assert_eq!(job.job_id, "ota-1");
        assert_eq!(
            job_status("IN_PROGRESS", 3, "downloading")["expectedVersion"],
            3
        );
        assert!(parse_next_job(payload, 200, "0.1.0").is_err());
        assert!(parse_next_job(payload, 100, "0.2.0").is_err());
    }

    #[test]
    fn shadow_rejects_unsafe_or_partial_configuration() {
        assert!(parse_shadow_document(
            br#"{"version":1,"state":{"sampleIntervalSeconds":1,"temperatureOffsetF":0}}"#
        )
        .is_err());
        assert!(
            parse_shadow_document(br#"{"version":1,"state":{"sampleIntervalSeconds":60}}"#)
                .is_err()
        );
        let parsed = parse_shadow_document(br#"{"version":1,"state":{"sampleIntervalSeconds":60,"temperatureOffsetF":0,"heater":true}}"#)
            .unwrap().unwrap();
        assert_eq!(parsed.unsupported_keys, ["heater"]);
        assert_eq!(parsed.configuration.neo_pixel_color, NeoPixelColor::Off);
        assert!(parse_shadow_document(br#"{"version":1,"state":{"sampleIntervalSeconds":60,"temperatureOffsetF":0,"neoPixelColor":"orange"}}"#).is_err());
    }
}
