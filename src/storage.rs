//! CSV logging helpers on top of the FAT filesystem mounted at `/sdcard`.
//!
//! The SD card itself is mounted in `main.rs` (it owns the SPI peripheral + pins);
//! once mounted, the filesystem is just the VFS path `/sdcard`, so everything here
//! is ordinary `std::fs`. All access is serialized by the caller via the SD mutex.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
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

/// Open a day's CSV file for streaming. `date` is "YYYY-MM-DD".
pub fn open_csv(date: &str) -> anyhow::Result<File> {
    let path = log_path(date);
    Ok(File::open(path)?)
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
