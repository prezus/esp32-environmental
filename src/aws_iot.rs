//! AWS IoT Core prototype transport over MQTT/TLS 443 with ALPN.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::aws_spool::AwsSpool;
use crate::neopixel::SharedNeoPixel;
use crate::{neopixel, ota, Calibration};
use anyhow::{bail, Context};
use environmental_core::{
    job_status, parse_application_ack, parse_next_job, parse_shadow_document, reported_shadow,
};
use esp_idf_svc::io::Write;
use esp_idf_svc::tls::{Config as TlsConfig, EspTls, InternalSocket, X509};

const PORT: u16 = 443;
const ALPN: &str = "x-amzn-mqtt-ca";
const MAX_PACKET_BYTES: usize = 16 * 1024;
const MAX_QUEUED_MESSAGES: usize = 8;

#[derive(Debug, Clone)]
pub struct AwsIotConfig {
    pub endpoint: String,
    pub client_id: String,
    pub thing_name: String,
    pub device_id: String,
    pub certificate_pem: &'static [u8],
    pub private_key_pem: &'static [u8],
}

impl AwsIotConfig {
    pub fn embedded(
        endpoint: &str,
        device_id: &str,
        certificate_pem: &'static [u8],
        private_key_pem: &'static [u8],
    ) -> anyhow::Result<Self> {
        let config = Self {
            endpoint: endpoint.into(),
            client_id: device_id.into(),
            thing_name: device_id.into(),
            device_id: device_id.into(),
            certificate_pem,
            private_key_pem,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        for (name, value) in [
            ("endpoint", self.endpoint.as_str()),
            ("clientId", self.client_id.as_str()),
            ("thingName", self.thing_name.as_str()),
            ("deviceId", self.device_id.as_str()),
        ] {
            if value.is_empty() || value.as_bytes().contains(&0) {
                bail!("AWS IoT {name} is empty or contains NUL");
            }
        }
        for (name, value) in [
            ("certificatePem", self.certificate_pem),
            ("privateKeyPem", self.private_key_pem),
        ] {
            if value.is_empty() || value.contains(&0) {
                bail!("AWS IoT {name} is empty or contains NUL");
            }
        }
        if self.endpoint.contains('/') || self.endpoint.contains(':') {
            bail!("AWS IoT endpoint must be a hostname");
        }
        if self.client_id != self.thing_name || self.device_id != self.thing_name {
            bail!("prototype clientId, thingName, and deviceId must match");
        }
        Ok(())
    }

    fn telemetry_topic(&self) -> String {
        format!("mothership/devices/{}/telemetry", self.device_id)
    }
    fn acknowledgement_topic(&self) -> String {
        format!("mothership/devices/{}/ack", self.device_id)
    }
    fn lwt_topic(&self) -> String {
        format!("mothership/devices/{}/lwt", self.device_id)
    }
    fn shadow_delta_topic(&self) -> String {
        format!("$aws/things/{}/shadow/update/delta", self.thing_name)
    }
    fn shadow_get_topic(&self) -> String {
        format!("$aws/things/{}/shadow/get", self.thing_name)
    }
    fn shadow_get_accepted_topic(&self) -> String {
        format!("$aws/things/{}/shadow/get/accepted", self.thing_name)
    }
    fn shadow_update_topic(&self) -> String {
        format!("$aws/things/{}/shadow/update", self.thing_name)
    }
    fn jobs_next_get_topic(&self) -> String {
        format!("$aws/things/{}/jobs/$next/get", self.thing_name)
    }
    fn jobs_next_accepted_topic(&self) -> String {
        format!("$aws/things/{}/jobs/$next/get/accepted", self.thing_name)
    }
    fn job_update_topic(&self, job_id: &str) -> String {
        format!("$aws/things/{}/jobs/{job_id}/update", self.thing_name)
    }
}

pub fn start_delivery_worker(
    config: AwsIotConfig,
    spool: AwsSpool,
    sample_interval_seconds: Arc<AtomicU32>,
    calibration: Calibration,
    neo_pixel: SharedNeoPixel,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("aws-iot-prototype".into())
        .stack_size(24 * 1024)
        .spawn(move || {
            delivery_worker(
                config,
                spool,
                sample_interval_seconds,
                calibration,
                neo_pixel,
            );
        })
        .context("failed to spawn AWS IoT worker")
}

fn delivery_worker(
    config: AwsIotConfig,
    spool: AwsSpool,
    sample_interval_seconds: Arc<AtomicU32>,
    calibration: Calibration,
    neo_pixel: SharedNeoPixel,
) {
    loop {
        let result = run_session(
            &config,
            &spool,
            &sample_interval_seconds,
            &calibration,
            &neo_pixel,
        );
        if let Err(error) = result {
            log::warn!("AWS IoT session failed; local SD logging continues: {error:#}");
        }
        std::thread::sleep(Duration::from_secs(15));
    }
}

fn run_session(
    config: &AwsIotConfig,
    spool: &AwsSpool,
    sample_interval_seconds: &AtomicU32,
    calibration: &Calibration,
    neo_pixel: &SharedNeoPixel,
) -> anyhow::Result<()> {
    let mut client = MqttClient::connect(config.clone())?;
    client.subscribe()?;
    client.publish_status(true)?;
    client.publish_qos1(&config.shadow_get_topic(), b"", false)?;
    loop {
        let (topic, payload) = client.read_publish()?;
        let is_get_response = topic == config.shadow_get_accepted_topic();
        process_control(
            &mut client,
            &topic,
            &payload,
            sample_interval_seconds,
            calibration,
            neo_pixel,
        )?;
        if is_get_response {
            break;
        }
    }
    client.publish_qos1(&config.jobs_next_get_topic(), b"{}", false)?;
    loop {
        let (topic, payload) = client.read_publish()?;
        let is_jobs_response = topic == config.jobs_next_accepted_topic();
        process_control(
            &mut client,
            &topic,
            &payload,
            sample_interval_seconds,
            calibration,
            neo_pixel,
        )?;
        if is_jobs_response {
            break;
        }
    }

    for record in spool.pending(8)? {
        client.publish_telemetry(&record.payload)?;
        loop {
            let (topic, payload) = client.read_publish()?;
            if let Some(event_id) = process_control(
                &mut client,
                &topic,
                &payload,
                sample_interval_seconds,
                calibration,
                neo_pixel,
            )? {
                if event_id == record.event_id {
                    spool.acknowledge_head(&event_id)?;
                    break;
                }
            }
        }
    }
    client.publish_status(false)?;
    client.disconnect()
}

fn process_control(
    client: &mut MqttClient,
    topic: &str,
    payload: &[u8],
    sample_interval_seconds: &AtomicU32,
    calibration: &Calibration,
    neo_pixel: &SharedNeoPixel,
) -> anyhow::Result<Option<String>> {
    if topic == client.config.acknowledgement_topic() {
        return parse_application_ack(payload)
            .map(|ack| Some(ack.event_id))
            .map_err(anyhow::Error::msg);
    }
    if topic == client.config.jobs_next_accepted_topic() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch; refusing OTA")?
            .as_secs() as i64;
        let Some(job) =
            parse_next_job(payload, now, env!("CARGO_PKG_VERSION")).map_err(anyhow::Error::msg)?
        else {
            return Ok(None);
        };
        let update_topic = client.config.job_update_topic(&job.job_id);
        client.publish_qos1(
            &update_topic,
            &serde_json::to_vec(&job_status(
                "IN_PROGRESS",
                job.execution_version,
                "downloading signed image",
            ))?,
            false,
        )?;
        if let Err(error) = ota::install_signed_image(&job.document.image_url) {
            client.publish_qos1(
                &update_topic,
                &serde_json::to_vec(&job_status(
                    "FAILED",
                    job.execution_version + 1,
                    "signed image installation failed",
                ))?,
                false,
            )?;
            return Err(error);
        }
        client.publish_qos1(
            &update_topic,
            &serde_json::to_vec(&job_status(
                "SUCCEEDED",
                job.execution_version + 1,
                "signed image installed; rebooting",
            ))?,
            false,
        )?;
        unsafe { esp_idf_svc::sys::esp_restart() }
    }
    if topic == client.config.shadow_delta_topic()
        || topic == client.config.shadow_get_accepted_topic()
    {
        let Some(delta) = parse_shadow_document(payload).map_err(anyhow::Error::msg)? else {
            return Ok(None);
        };
        if !delta.unsupported_keys.is_empty() {
            let report = serde_json::json!({"state":{"reported":{"schemaVersion":1,"configurationError":
                format!("unsupported desired keys: {}", delta.unsupported_keys.join(",")),"desiredVersionRejected":delta.version}}});
            client.publish_shadow(&report)?;
            return Ok(None);
        }
        sample_interval_seconds.store(
            delta.configuration.sample_interval_seconds as u32,
            Ordering::Release,
        );
        neopixel::set_color(neo_pixel, delta.configuration.neo_pixel_color)?;
        *calibration
            .lock()
            .map_err(|_| anyhow::anyhow!("calibration mutex poisoned"))? =
            delta.configuration.temperature_offset_f;
        client.publish_shadow(&reported_shadow(&delta.configuration, delta.version))?;
        return Ok(None);
    }
    bail!("message arrived on unexpected AWS IoT topic {topic}")
}

struct MqttClient {
    tls: EspTls<InternalSocket>,
    next_packet_id: u16,
    config: AwsIotConfig,
    queued: VecDeque<(String, Vec<u8>)>,
    _certificate: Vec<u8>,
    _private_key: Vec<u8>,
}

impl MqttClient {
    fn connect(config: AwsIotConfig) -> anyhow::Result<Self> {
        let certificate = nul_terminated_pem(config.certificate_pem, "device certificate")?;
        let private_key = nul_terminated_pem(config.private_key_pem, "device private key")?;
        let mut tls = EspTls::new().context("failed to allocate ESP-TLS")?;
        let alpn = [ALPN];
        let tls_config = TlsConfig {
            alpn_protos: Some(&alpn),
            client_cert: Some(X509::pem_until_nul(&certificate)),
            client_key: Some(X509::pem_until_nul(&private_key)),
            timeout_ms: 10_000,
            ..TlsConfig::new()
        };
        tls.connect(&config.endpoint, PORT, &tls_config)
            .with_context(|| {
                format!("AWS IoT TLS handshake failed on TCP {PORT} with ALPN {ALPN}")
            })?;
        let mut client = Self {
            tls,
            next_packet_id: 1,
            config,
            queued: VecDeque::new(),
            _certificate: certificate,
            _private_key: private_key,
        };
        client.send_connect()?;
        Ok(client)
    }

    fn send_connect(&mut self) -> anyhow::Result<()> {
        let mut body = Vec::new();
        push_utf8(&mut body, "MQTT")?;
        // AWS IoT's 1,200-second maximum covers the one-minute ingestion ACK
        // and a bounded signed-image download without a second socket task.
        body.extend_from_slice(&[4, 0x2c, 4, 176]);
        push_utf8(&mut body, &self.config.client_id)?;
        push_utf8(&mut body, &self.config.lwt_topic())?;
        push_utf8(&mut body, r#"{"online":false}"#)?;
        self.write_packet(0x10, &body)?;
        let (header, response) = self.read_packet()?;
        if header >> 4 != 2 || response.len() != 2 || response[1] != 0 {
            bail!("AWS IoT MQTT CONNECT rejected");
        }
        Ok(())
    }

    fn subscribe(&mut self) -> anyhow::Result<()> {
        let topics = [
            self.config.shadow_delta_topic(),
            self.config.shadow_get_accepted_topic(),
            self.config.acknowledgement_topic(),
            self.config.jobs_next_accepted_topic(),
        ];
        let packet_id = self.take_packet_id();
        let mut body = packet_id.to_be_bytes().to_vec();
        for topic in &topics {
            push_utf8(&mut body, topic)?;
            body.push(1);
        }
        self.write_packet(0x82, &body)?;
        loop {
            let (header, response) = self.read_packet()?;
            match header >> 4 {
                9 if response.get(..2) == Some(&packet_id.to_be_bytes()) => {
                    if response.len() != topics.len() + 2
                        || response[2..].iter().any(|code| !matches!(code, 0 | 1))
                    {
                        bail!("AWS IoT rejected MQTT subscription: {:?}", &response[2..]);
                    }
                    return Ok(());
                }
                3 => self.queue_publish(header, &response)?,
                13 => {}
                _ => {}
            }
        }
    }

    fn publish_status(&mut self, online: bool) -> anyhow::Result<()> {
        self.publish_qos1(
            &self.config.lwt_topic(),
            if online {
                br#"{"online":true}"#
            } else {
                br#"{"online":false}"#
            },
            true,
        )
    }

    fn publish_shadow(&mut self, report: &serde_json::Value) -> anyhow::Result<()> {
        self.publish_qos1(
            &self.config.shadow_update_topic(),
            &serde_json::to_vec(report)?,
            false,
        )
    }

    fn publish_telemetry(&mut self, payload: &[u8]) -> anyhow::Result<()> {
        self.publish_qos1(&self.config.telemetry_topic(), payload, false)
    }

    fn publish_qos1(&mut self, topic: &str, payload: &[u8], retain: bool) -> anyhow::Result<()> {
        let packet_id = self.take_packet_id();
        let mut body = Vec::new();
        push_utf8(&mut body, topic)?;
        body.extend_from_slice(&packet_id.to_be_bytes());
        body.extend_from_slice(payload);
        self.write_packet(if retain { 0x33 } else { 0x32 }, &body)?;
        loop {
            let (header, response) = self.read_packet()?;
            match header >> 4 {
                4 if response.get(..2) == Some(&packet_id.to_be_bytes()) => return Ok(()),
                3 => self.queue_publish(header, &response)?,
                13 => {}
                _ => {}
            }
        }
    }

    fn read_publish(&mut self) -> anyhow::Result<(String, Vec<u8>)> {
        if let Some(message) = self.queued.pop_front() {
            return Ok(message);
        }
        loop {
            let (header, packet) = self.read_packet()?;
            match header >> 4 {
                3 => return self.decode_publish(header, &packet),
                13 => continue,
                kind => bail!("expected MQTT PUBLISH, received packet type {kind}"),
            }
        }
    }

    fn queue_publish(&mut self, header: u8, packet: &[u8]) -> anyhow::Result<()> {
        if self.queued.len() >= MAX_QUEUED_MESSAGES {
            bail!("MQTT control queue is full");
        }
        let decoded = self.decode_publish(header, packet)?;
        self.queued.push_back(decoded);
        Ok(())
    }

    fn decode_publish(&mut self, header: u8, packet: &[u8]) -> anyhow::Result<(String, Vec<u8>)> {
        let qos = (header >> 1) & 0x03;
        if qos > 1 {
            bail!("unsupported MQTT PUBLISH QoS {qos}");
        }
        let mut offset = 0;
        let topic = take_utf8(packet, &mut offset)?.to_owned();
        if qos == 1 {
            let id = packet
                .get(offset..offset + 2)
                .context("truncated MQTT packet ID")?;
            offset += 2;
            self.write_packet(0x40, id)?;
        }
        Ok((topic, packet[offset..].to_vec()))
    }

    fn disconnect(&mut self) -> anyhow::Result<()> {
        self.write_packet(0xe0, &[])
    }

    fn take_packet_id(&mut self) -> u16 {
        let id = self.next_packet_id;
        self.next_packet_id = self.next_packet_id.wrapping_add(1).max(1);
        id
    }

    fn write_packet(&mut self, header: u8, body: &[u8]) -> anyhow::Result<()> {
        if body.len() > MAX_PACKET_BYTES {
            bail!("MQTT packet exceeds limit");
        }
        let mut packet = vec![header];
        encode_remaining_length(body.len(), &mut packet);
        packet.extend_from_slice(body);
        write_all(&mut self.tls, &packet)
    }

    fn read_packet(&mut self) -> anyhow::Result<(u8, Vec<u8>)> {
        let header = read_byte(&mut self.tls)?;
        let mut multiplier = 1usize;
        let mut remaining = 0usize;
        for _ in 0..4 {
            let byte = read_byte(&mut self.tls)?;
            remaining = remaining
                .checked_add(((byte & 0x7f) as usize) * multiplier)
                .context("MQTT length overflow")?;
            if byte & 0x80 == 0 {
                if remaining > MAX_PACKET_BYTES {
                    bail!("incoming MQTT packet exceeds limit");
                }
                let mut body = vec![0; remaining];
                read_exact(&mut self.tls, &mut body)?;
                return Ok((header, body));
            }
            multiplier *= 128;
        }
        bail!("invalid MQTT remaining length")
    }
}

fn nul_terminated_pem(pem: &[u8], description: &str) -> anyhow::Result<Vec<u8>> {
    if pem.is_empty() || pem.contains(&0) {
        bail!("{description} is empty or contains NUL");
    }
    let mut bytes = pem.to_vec();
    bytes.push(0);
    Ok(bytes)
}

fn push_utf8(target: &mut Vec<u8>, value: &str) -> anyhow::Result<()> {
    let length = u16::try_from(value.len()).context("MQTT UTF-8 value too long")?;
    target.extend_from_slice(&length.to_be_bytes());
    target.extend_from_slice(value.as_bytes());
    Ok(())
}

fn take_utf8<'a>(packet: &'a [u8], offset: &mut usize) -> anyhow::Result<&'a str> {
    let length = packet
        .get(*offset..*offset + 2)
        .context("truncated MQTT string length")?;
    *offset += 2;
    let length = u16::from_be_bytes([length[0], length[1]]) as usize;
    let bytes = packet
        .get(*offset..*offset + length)
        .context("truncated MQTT string")?;
    *offset += length;
    core::str::from_utf8(bytes).context("invalid MQTT UTF-8")
}

fn encode_remaining_length(mut length: usize, target: &mut Vec<u8>) {
    loop {
        let mut byte = (length % 128) as u8;
        length /= 128;
        if length > 0 {
            byte |= 0x80;
        }
        target.push(byte);
        if length == 0 {
            break;
        }
    }
}

fn write_all(writer: &mut EspTls<InternalSocket>, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => bail!("TLS write returned zero"),
            Ok(written) => bytes = &bytes[written..],
            Err(error) if is_tls_retry(error.code()) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => bail!("TLS write failed: {error:?}"),
        }
    }
    loop {
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(error) if is_tls_retry(error.0.code()) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => bail!("TLS flush failed: {error:?}"),
        }
    }
}

fn read_exact(reader: &mut EspTls<InternalSocket>, mut bytes: &mut [u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        match reader.read(bytes) {
            Ok(0) => bail!("TLS connection closed"),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if is_tls_retry(error.code()) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => bail!("TLS read failed: {error:?}"),
        }
    }
    Ok(())
}

fn read_byte(reader: &mut EspTls<InternalSocket>) -> anyhow::Result<u8> {
    let mut byte = [0];
    read_exact(reader, &mut byte)?;
    Ok(byte[0])
}

fn is_tls_retry(code: i32) -> bool {
    matches!(code, -0x6900 | -0x6880)
}
