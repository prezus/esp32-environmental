# esp32-environmental

Temperature + humidity logger built on an **Adafruit ESP32-S3 Feather**, an **Adalogger
FeatherWing** (microSD + RTC), and a **Sensirion SHT45** sensor. Samples are written to
the SD card as daily CSV files and served over WiFi as a live dashboard with CSV export.

Written in Rust on the `std` / ESP-IDF stack, built with [`just`](https://github.com/casey/just).

## Hardware

| Part | Role | Bus |
|------|------|-----|
| [ESP32-S3 Feather 4MB/2MB (#5477)](https://www.adafruit.com/product/5477) | MCU + WiFi | — |
| [Adalogger FeatherWing (#2922)](https://www.adafruit.com/product/2922) | microSD + PCF8523 RTC | SD on SPI, RTC on I²C (`0x68`) |
| [Sensirion SHT45 (#6174)](https://www.adafruit.com/product/6174) | Temp/Humidity | I²C (`0x44`), STEMMA QT |

Stack the FeatherWing on the Feather with headers; plug the SHT45 into the STEMMA QT port.
Install a CR1220 cell in the Adalogger so the RTC keeps time across power loss, and insert
a microSD card.

### Pin assumptions — verify before flashing

The pin map lives at the top of [`src/main.rs`](src/main.rs). Defaults for the S3 Feather:

All values below are confirmed against Adafruit's ESP32-S3 Feather board definition.

| Signal | GPIO | Notes |
|--------|------|-------|
| I²C SDA / SCL | 3 / 4 | SHT45 + RTC share this bus |
| I²C power rail | 7 | `I2C_POWER`; driven high to power the STEMMA QT port |
| SPI SCK / MOSI / MISO | 36 / 35 / 37 | SD card |
| SD chip-select | 10 | Adalogger default CS (the `D10` header pin) |

The only hardware-dependent one is the **SD chip-select**: the Adalogger ships with its
CS trace jumpered to `D10` (= GPIO10), which matches. If SD mounting fails, check that
the Adalogger's CS solder jumper hasn't been cut/rewired to a different pin.

## Prerequisites

The ESP32-S3 is an **Xtensa** core, so it needs the `esp` Rust toolchain (not stable):

```sh
cargo install espup espflash ldproxy
espup install            # installs the `esp` toolchain + GCC; writes ~/export-esp.sh
```

`just` sources `~/export-esp.sh` automatically for each build recipe.

## Configure

```sh
cp cfg.toml.example cfg.toml      # then edit cfg.toml
```

```toml
[esp32-environmental]
wifi_ssid = "YOUR_WIFI_SSID"
wifi_psk  = "YOUR_WIFI_PASSWORD"
sample_interval_secs = 30
hostname  = "esp32-env"   # reserved for future mDNS; unused today
```

`cfg.toml` is gitignored. Leave `wifi_ssid` empty to run offline (RTC timestamps only,
no dashboard).

## Build / flash / monitor

```sh
just build      # compile (first build downloads + builds ESP-IDF; takes a while)
just flash      # compile, flash, and open the serial monitor
just monitor    # just the serial monitor
```

On boot the serial log prints the assigned IP (`WiFi connected: dashboard at
http://<ip>/`). Browse to that address to see the dashboard.

> mDNS (`esp32-env.local`) is not enabled by default — in ESP-IDF v5.x mDNS is a
> managed component, so it would require adding an `idf_component.yml` dependency on
> `espressif/mdns` plus a small `EspMdns` setup. The `hostname` config field is
> reserved for that. For now, use the IP from the serial log (or your router's
> client list / a static DHCP lease).

## Data format

Daily CSV files on the card under `/sdcard/logs/`, e.g. `2026-06-26.csv`. Timestamps
are local **Mountain Time** (MST/MDT) with offset; `unix_ts` is UTC epoch seconds.

```csv
iso8601,unix_ts,temp_c,temp_f,humidity_pct
2026-06-26T08:03:22-06:00,1782656602,22.41,72.34,48.10
```

CSV opens directly in Excel / Sheets / pandas. The dashboard also exposes:

| Endpoint | Returns |
|----------|---------|
| `GET /` | dashboard (live cards + separate temp/humidity charts + day picker) |
| `GET /chart.js` | Chart.js, served from the device (vendored in `assets/`, no CDN) |
| `GET /api/latest` | latest reading as JSON (`temp_c`, `temp_f`, `humidity_pct`) |
| `GET /api/files` | available log dates as JSON |
| `GET /api/data?date=YYYY-MM-DD` | a day's points as JSON (for the charts) |
| `GET /download?date=YYYY-MM-DD` | raw CSV download (export) |

The dashboard's charting library (`assets/chart.umd.min.js`) is embedded into the firmware
and served locally, so the dashboard works with no internet access.

## Wipe the SD card

Easiest first:

1. **Dashboard** — click the **Wipe logs** button (asks for confirmation).
2. **HTTP** — `curl -X POST http://<device-ip>/wipe` from any machine on the network.
3. **Serial** (no network needed) — type `WIPE` + Enter in `just monitor`.
4. **Physical** — power off, eject the card, and reformat on a computer as **FAT32 /
   MS-DOS (FAT)** with an **MBR** scheme (not exFAT/APFS). Use this only if the
   filesystem itself is corrupted.

Options 1–3 delete every `.csv` under `/sdcard/logs/`; the card stays in the device.

## How it works

- **Sampling loop** (main task): read SHT45 → timestamp from the PCF8523 RTC → append a CSV row → update the shared "latest reading".
- **HTTP server** (background tasks): serves the dashboard, JSON API, and CSV downloads.
- **Serial listener** (background thread): handles the `WIPE` command.
- **Time**: on boot with WiFi, SNTP syncs the system clock and writes it into the RTC. The RTC (battery-backed) then provides timestamps even offline.

All SD access is serialized by a mutex so the writer and HTTP readers don't collide.
