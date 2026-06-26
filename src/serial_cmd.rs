//! USB-serial command loop. Reads lines from the console (stdin over USB-Serial-JTAG)
//! and acts on them. Currently the only command is `WIPE`, which deletes all log files.
//!
//! `just wipe-sd` sends "WIPE" to the serial port to trigger this.

use std::io::Read;

use esp_idf_svc::hal::delay::FreeRtos;

use crate::storage;
use crate::SdGuard;

/// Spawn the background thread that listens for serial commands.
pub fn spawn(sd: SdGuard) {
    std::thread::Builder::new()
        .stack_size(6 * 1024)
        .name("serial-cmd".into())
        .spawn(move || run(sd))
        .expect("spawn serial-cmd thread");
}

fn run(sd: SdGuard) {
    // stdin over USB-Serial-JTAG is non-blocking: a read with no data pending returns
    // WouldBlock / EAGAIN rather than blocking. So we poll one byte at a time, treat
    // "no data" as silence (no logging), and assemble a line until newline.
    let mut stdin = std::io::stdin();
    let mut line = String::new();
    let mut byte = [0u8; 1];
    loop {
        match stdin.read(&mut byte) {
            Ok(1) => match byte[0] {
                b'\n' | b'\r' => {
                    if !line.is_empty() {
                        handle(line.trim(), &sd);
                        line.clear();
                    }
                }
                b => {
                    if line.len() < 64 {
                        line.push(b as char);
                    }
                }
            },
            // EOF or no data available yet — wait quietly and poll again.
            Ok(_) => FreeRtos::delay_ms(100),
            Err(_) => FreeRtos::delay_ms(100),
        }
    }
}

fn handle(cmd: &str, sd: &SdGuard) {
    match cmd.to_ascii_uppercase().as_str() {
        "" => {}
        "WIPE" => {
            let _g = sd.lock().unwrap();
            match storage::wipe() {
                Ok(n) => println!("WIPE: removed {n} log file(s)"),
                Err(e) => println!("WIPE: failed: {e}"),
            }
        }
        "HELP" => println!("commands: WIPE (delete all logs), HELP"),
        other => println!("unknown command: {other:?} (try HELP)"),
    }
}
