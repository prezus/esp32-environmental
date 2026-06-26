//! CSV logging helpers on top of the FAT filesystem mounted at `/sdcard`.
//!
//! The SD card itself is mounted in `main.rs` (it owns the SPI peripheral + pins);
//! once mounted, the filesystem is just the VFS path `/sdcard`, so everything here
//! is ordinary `std::fs`. All access is serialized by the caller via the SD mutex.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use crate::rtc::Timestamp;
use crate::sensor::Sample;

/// VFS mount point for the SD card.
pub const MOUNT_POINT: &str = "/sdcard";
/// Directory holding the daily CSV files.
pub const LOG_DIR: &str = "/sdcard/logs";

const CSV_HEADER: &str = "iso8601,unix_ts,temp_c,temp_f,humidity_pct\n";

/// Make sure `/sdcard/logs` exists. Call once after mounting.
pub fn init() -> anyhow::Result<()> {
    fs::create_dir_all(LOG_DIR)?;
    Ok(())
}

/// Append one sample to the current day's CSV file, writing the header if the file
/// is new.
pub fn append(ts: &Timestamp, sample: &Sample) -> anyhow::Result<()> {
    let path = log_path(&ts.date);
    let is_new = !Path::new(&path).exists();

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    if is_new {
        file.write_all(CSV_HEADER.as_bytes())?;
    }
    writeln!(
        file,
        "{},{},{:.2},{:.2},{:.2}",
        ts.iso8601, ts.unix, sample.temperature_c, sample.temperature_f, sample.humidity_pct
    )?;
    file.flush()?;
    Ok(())
}

/// List available log dates (the "YYYY-MM-DD" stems), newest first.
pub fn list_dates() -> anyhow::Result<Vec<String>> {
    let mut dates = Vec::new();
    if let Ok(entries) = fs::read_dir(LOG_DIR) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".csv") {
                dates.push(stem.to_string());
            }
        }
    }
    dates.sort();
    dates.reverse();
    Ok(dates)
}

/// Read a whole day's CSV file as a string. `date` is "YYYY-MM-DD".
pub fn read_csv(date: &str) -> anyhow::Result<String> {
    let path = log_path(date);
    Ok(fs::read_to_string(path)?)
}

/// Return the data rows (skipping the header) of a day's file as
/// `(iso8601, temp_c, temp_f, humidity_pct)` tuples — used to build the chart JSON.
pub fn read_rows(date: &str) -> anyhow::Result<Vec<(String, f32, f32, f32)>> {
    let path = log_path(date);
    let file = File::open(path)?;
    let mut rows = Vec::new();
    for line in BufReader::new(file).lines().skip(1) {
        let line = line?;
        let mut cols = line.split(',');
        let iso = cols.next().unwrap_or_default().to_string();
        let _unix = cols.next();
        let tc = cols.next().and_then(|s| s.parse().ok()).unwrap_or(f32::NAN);
        let tf = cols.next().and_then(|s| s.parse().ok()).unwrap_or(f32::NAN);
        let rh = cols.next().and_then(|s| s.parse().ok()).unwrap_or(f32::NAN);
        if !iso.is_empty() {
            rows.push((iso, tc, tf, rh));
        }
    }
    Ok(rows)
}

/// Delete every `.csv` file under the log directory. Returns how many were removed.
pub fn wipe() -> anyhow::Result<usize> {
    let mut removed = 0;
    if let Ok(entries) = fs::read_dir(LOG_DIR) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("csv") {
                fs::remove_file(&path)?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Reject anything that isn't a plain "YYYY-MM-DD" stem, so HTTP query input can't
/// escape the log directory.
pub fn is_valid_date(date: &str) -> bool {
    date.len() == 10
        && date.as_bytes().iter().enumerate().all(|(i, &b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        })
}

fn log_path(date: &str) -> String {
    format!("{LOG_DIR}/{date}.csv")
}
