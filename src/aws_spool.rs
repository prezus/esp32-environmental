use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context};
use environmental_core::TelemetryEnvelope;

const SPOOL_PATH: &str = "/sdcard/aws-iot-pending.jsonl";
const CURSOR_PATH: &str = "/sdcard/aws-iot-cursor";
const SEQUENCE_PATH: &str = "/sdcard/aws-iot-sequence";
const MAX_SPOOL_BYTES: u64 = 64 * 1024 * 1024;

pub struct PendingRecord {
    pub event_id: String,
    pub payload: Vec<u8>,
}

#[derive(Clone)]
pub struct AwsSpool {
    sd_guard: Arc<Mutex<()>>,
}

impl AwsSpool {
    pub fn new(sd_guard: Arc<Mutex<()>>) -> Self {
        Self { sd_guard }
    }

    /// Reserve a never-reused device-global sequence before writing telemetry.
    pub fn reserve_sequence(&self) -> anyhow::Result<u64> {
        let _guard = self
            .sd_guard
            .lock()
            .map_err(|_| anyhow::anyhow!("SD mutex poisoned"))?;
        let current = read_number(SEQUENCE_PATH)?.unwrap_or(0);
        let next = current
            .checked_add(1)
            .context("AWS telemetry sequence exhausted")?;
        append_number(SEQUENCE_PATH, next)?;
        Ok(current)
    }

    /// Append and sync before any MQTT attempt so broker success cannot outrun durability.
    pub fn enqueue(&self, envelope: &TelemetryEnvelope) -> anyhow::Result<()> {
        let _guard = self
            .sd_guard
            .lock()
            .map_err(|_| anyhow::anyhow!("SD mutex poisoned"))?;
        repair_torn_tail_at(SPOOL_PATH)?;
        let spool_bytes = match fs::metadata(SPOOL_PATH) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        if spool_bytes >= MAX_SPOOL_BYTES {
            bail!("AWS spool reached its 64 MiB safety limit; preserving oldest records");
        }
        let bytes = serde_json::to_vec(envelope)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(SPOOL_PATH)?;
        file.write_all(envelope.event_id.as_bytes())?;
        file.write_all(b"\t")?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }

    /// Read a bounded oldest-first batch without loading the full offline backlog.
    pub fn pending(&self, limit: usize) -> anyhow::Result<Vec<PendingRecord>> {
        let _guard = self
            .sd_guard
            .lock()
            .map_err(|_| anyhow::anyhow!("SD mutex poisoned"))?;
        let mut reader = match open_at_cursor()? {
            Some(reader) => reader,
            None => return Ok(Vec::new()),
        };
        let mut records = Vec::new();
        while records.len() < limit {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            if !line.ends_with('\n') {
                break;
            }
            let raw = line.trim_end_matches('\n');
            let (event_id, payload) = match raw.split_once('\t') {
                Some(parts) => parts,
                None => (
                    legacy_event_id(raw).context("invalid AWS spool record")?,
                    raw,
                ),
            };
            if event_id.is_empty() || payload.is_empty() {
                bail!("invalid AWS spool record");
            }
            records.push(PendingRecord {
                event_id: event_id.to_owned(),
                payload: payload.as_bytes().to_vec(),
            });
        }
        Ok(records)
    }

    /// Advance only past the acknowledged head. A crash before the cursor write
    /// replays a duplicate; it cannot lose an unacknowledged record.
    pub fn acknowledge_head(&self, event_id: &str) -> anyhow::Result<()> {
        let _guard = self
            .sd_guard
            .lock()
            .map_err(|_| anyhow::anyhow!("SD mutex poisoned"))?;
        let mut reader =
            open_at_cursor()?.context("application ACK arrived with an empty spool")?;
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if !line.ends_with('\n') {
            bail!("application ACK arrived for a torn spool record");
        }
        let raw = line.trim_end_matches('\n');
        let stored_event_id = match raw.split_once('\t') {
            Some((stored_event_id, _)) => stored_event_id,
            None => legacy_event_id(raw).context("invalid AWS spool record")?,
        };
        if stored_event_id != event_id {
            bail!("application ACK does not match the oldest durable event");
        }
        write_cursor(reader.stream_position()?)
    }
}

fn open_at_cursor() -> anyhow::Result<Option<BufReader<File>>> {
    let file = match File::open(SPOOL_PATH) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let length = file.metadata()?.len();
    let mut cursor = read_number(CURSOR_PATH)?.unwrap_or(0);
    if cursor > length {
        // Compaction writes cursor zero before replacing the file. This fallback
        // is defensive and chooses duplicate replay over skipping pending data.
        cursor = 0;
        write_cursor(0)?;
    }
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(cursor))?;
    Ok(Some(reader))
}

fn repair_torn_tail_at(path: &str) -> anyhow::Result<()> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut position = file.metadata()?.len();
    if position == 0 {
        return Ok(());
    }
    let mut byte = [0];
    file.seek(SeekFrom::Start(position - 1))?;
    file.read_exact(&mut byte)?;
    if byte[0] == b'\n' {
        return Ok(());
    }
    while position > 0 {
        position -= 1;
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            position += 1;
            break;
        }
    }
    file.set_len(position)?;
    file.sync_data()?;
    Ok(())
}

fn legacy_event_id(record: &str) -> Option<&str> {
    let remainder = record.split_once("\"eventId\":\"")?.1;
    let event_id = remainder.split_once('"')?.0;
    (!event_id.is_empty()).then_some(event_id)
}

fn read_number(path: &str) -> anyhow::Result<Option<u64>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut reader = BufReader::new(file);
    let mut last = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if !line.ends_with('\n') {
            break;
        }
        last = Some(
            line.trim_end_matches('\n')
                .parse::<u64>()
                .context("invalid AWS spool metadata")?,
        );
    }
    Ok(last)
}

fn write_cursor(cursor: u64) -> anyhow::Result<()> {
    append_number(CURSOR_PATH, cursor)
}

fn append_number(path: &str, value: u64) -> anyhow::Result<()> {
    repair_torn_tail_at(path)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{value}")?;
    file.sync_data()?;
    Ok(())
}
