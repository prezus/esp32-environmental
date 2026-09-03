use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context};
use environmental_core::TelemetryEnvelope;

const SPOOL_PATH: &str = "/sdcard/aws-iot-pending.jsonl";
const SPOOL_TEMP_PATH: &str = "/sdcard/aws-iot-pending.jsonl.tmp";
const CURSOR_PATH: &str = "/sdcard/aws-iot-cursor";
const CURSOR_TEMP_PATH: &str = "/sdcard/aws-iot-cursor.tmp";
const SEQUENCE_PATH: &str = "/sdcard/aws-iot-sequence";
const SEQUENCE_TEMP_PATH: &str = "/sdcard/aws-iot-sequence.tmp";
const MAX_SPOOL_BYTES: u64 = 64 * 1024 * 1024;
const COMPACT_AFTER_BYTES: u64 = 1024 * 1024;

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
        write_atomic(
            SEQUENCE_TEMP_PATH,
            SEQUENCE_PATH,
            format!("{next}\n").as_bytes(),
        )?;
        Ok(current)
    }

    /// Append and sync before any MQTT attempt so broker success cannot outrun durability.
    pub fn enqueue(&self, envelope: &TelemetryEnvelope) -> anyhow::Result<()> {
        let _guard = self
            .sd_guard
            .lock()
            .map_err(|_| anyhow::anyhow!("SD mutex poisoned"))?;
        compact_if_needed()?;
        repair_torn_tail()?;
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
            let (event_id, payload) = line
                .trim_end_matches('\n')
                .split_once('\t')
                .context("invalid AWS spool record")?;
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
        let stored_event_id = line
            .trim_end_matches('\n')
            .split_once('\t')
            .map(|(stored_event_id, _)| stored_event_id)
            .context("invalid AWS spool record")?;
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

fn compact_if_needed() -> anyhow::Result<()> {
    let cursor = read_number(CURSOR_PATH)?.unwrap_or(0);
    if cursor < COMPACT_AFTER_BYTES {
        return Ok(());
    }
    let mut source = match File::open(SPOOL_PATH) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return write_cursor(0),
        Err(error) => return Err(error.into()),
    };
    source.seek(SeekFrom::Start(cursor.min(source.metadata()?.len())))?;
    let mut replacement = File::create(SPOOL_TEMP_PATH)?;
    std::io::copy(&mut source, &mut replacement)?;
    replacement.sync_data()?;
    drop(replacement);
    // Cursor first is deliberate: interruption can replay acknowledged rows,
    // which cloud idempotency tolerates, but can never skip pending rows.
    write_cursor(0)?;
    fs::rename(SPOOL_TEMP_PATH, SPOOL_PATH)?;
    Ok(())
}

fn repair_torn_tail() -> anyhow::Result<()> {
    let mut file = match OpenOptions::new().read(true).write(true).open(SPOOL_PATH) {
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

fn read_number(path: &str) -> anyhow::Result<Option<u64>> {
    match fs::read_to_string(path) {
        Ok(raw) => raw
            .trim()
            .parse::<u64>()
            .context("invalid AWS spool metadata")
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn write_cursor(cursor: u64) -> anyhow::Result<()> {
    write_atomic(
        CURSOR_TEMP_PATH,
        CURSOR_PATH,
        format!("{cursor}\n").as_bytes(),
    )
}

fn write_atomic(temporary: &str, destination: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = File::create(temporary)?;
    file.write_all(bytes)?;
    file.sync_data()?;
    drop(file);
    fs::rename(temporary, destination)?;
    Ok(())
}
