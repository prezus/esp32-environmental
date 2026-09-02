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
| `GET /api/config` | current temperature offset as JSON |
| `POST /api/config?temp_offset_f=<delta>` | set the temperature offset (°F) |

The dashboard's charting library (`assets/chart.umd.min.js`) is embedded into the firmware
and served locally, so the dashboard works with no internet access.

## Wipe the SD card

Power off, eject the card, and reformat it on a computer as **FAT32 / MS-DOS (FAT)**
with an **MBR** scheme (not exFAT/APFS — the firmware's FAT driver won't mount those).
On reinsertion the firmware recreates `/sdcard/logs/` and starts a fresh CSV.

> If the card is >32 GB, Disk Utility may only offer exFAT; use a ≤32 GB card or
> `diskutil eraseDisk FAT32 ESP32 MBRFormat /dev/diskN` (verify `diskN` first).

## Temperature calibration

The SHT45 sits near the warm ESP32, so it can read a few °F high (self-heating). The
dashboard has a **Temp calibration offset (°F)** field — enter a delta (e.g. `-4.5`),
click **Save**, and it's applied to all new readings and persisted in NVS (survives
reboots and card reformats). It does not rewrite already-logged rows. Also settable via
`POST /api/config?temp_offset_f=<delta>`. Prefer physically moving the sensor away from
the board first; use the offset to fine-tune.

## How it works

- **Sampling loop** (main task): read SHT45 → apply the calibration offset → timestamp from the PCF8523 RTC → append a CSV row → update the shared "latest reading".
- **HTTP server** (background tasks): serves the dashboard, JSON API, CSV downloads, and the calibration config endpoint.
- **Time**: on boot with WiFi, SNTP syncs the system clock and writes it into the RTC. The RTC (battery-backed) then provides timestamps even offline.

All SD access is serialized by a mutex so the writer and HTTP readers don't collide.

## AWS IoT Core prototype branch

The `prototype/aws-iot-core` branch adds a generic prototype identity,
`mothership-prototype-environment-monitor-01`, without associating it with a
production location. AWS is off by default. When enabled in ignored `cfg.toml`,
the device reads its endpoint and X.509 paths from `/sdcard/aws-iot.json`:

```toml
aws_iot_enabled = true
aws_iot_config_path = "/sdcard/aws-iot.json"
```

```json
{
  "endpoint": "your-endpoint-ats.iot.us-west-2.amazonaws.com",
  "clientId": "mothership-prototype-environment-monitor-01",
  "thingName": "mothership-prototype-environment-monitor-01",
  "deviceId": "mothership-prototype-environment-monitor-01",
  "certificatePath": "/sdcard/aws/device-certificate.pem",
  "privateKeyPath": "/sdcard/aws/device-private-key.pem"
}
```

Each RTC-stamped SHT45 sample remains in the existing daily CSV and is also
appended to an SD JSON-lines spool with a durable device-global sequence. MQTT
runs over TLS TCP 443 with `x-amzn-mqtt-ca`, a retained Last Will, a persistent
session, and per-device topics. MQTT PUBACK never removes a spool record; only a
matching accepted/duplicate application ACK after D1 commit removes its head.

Device Shadow desired state controls `sampleIntervalSeconds` (10–3600) and
`temperatureOffsetF` (-20–20). Values are applied and then reported; unknown or
partial configuration is rejected. AWS Jobs requests the next execution on
each session. An applicable `install` document downloads a signed image over
HTTPS into the inactive OTA slot, publishes execution status, and reboots.

The 4 MB flash layout contains two 1.875 MiB OTA slots; current flash sections
occupy about 1.29 MB. Signed-update mode uses the public key from the running
signed image, so the first image flashed must also be signed. Keep the RSA-3072
private key beneath `.prototype-secrets/` and build the artifact with:

```sh
scripts/build-signed-image.sh .prototype-secrets/ota-signing-key.pem
```

Do not use `just flash` for this branch until the explicit signed-initial-image
procedure has been reviewed and hardware flashing has been separately approved.
No connected-device, SD power-loss, MQTT heap, Jobs, signature, or rollback
claim is proven by a successful build.
