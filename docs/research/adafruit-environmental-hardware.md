# Adafruit environmental prototype hardware

The target assembly is fixed to:

- [Adafruit ESP32-S3 Feather 4 MB Flash / 2 MB PSRAM, product 5477](https://www.adafruit.com/product/5477)
- [Adalogger FeatherWing, product 2922](https://www.adafruit.com/product/2922), with CR1220 RTC battery and microSD installed
- [SHT45 with PTFE, product 6174](https://www.adafruit.com/product/6174), connected over STEMMA QT/I2C

## Verified interfaces

Adafruit documents product 5477 as the 4 MB flash, 2 MB PSRAM ESP32-S3 variant with native USB, Wi-Fi, and a switchable STEMMA QT rail. The rail must be driven from GPIO7 outside Arduino/CircuitPython. The official Arduino board definition maps SDA/SCL to GPIO3/GPIO4 and SPI MOSI/SCK/MISO to GPIO35/GPIO36/GPIO37. Sources: [Adafruit Feather guide](https://learn.adafruit.com/adafruit-esp32-s3-feather/overview), [pinouts](https://learn.adafruit.com/adafruit-esp32-s3-feather/pinouts), and [Espressif board definition](https://github.com/espressif/arduino-esp32/blob/3.3.10/variants/adafruit_feather_esp32s3/pins_arduino.h).

The Adalogger FeatherWing uses the Feather SPI header plus a separate SD chip-select and a PCF8523 RTC on shared I2C. Its RTC is battery-backed by CR1220. The assembled repository currently maps its unmodified D10 chip-select trace to GPIO10; this remains a first-hardware-test item because Adafruit's generic FeatherWing guide lists family-specific CS mappings. Source: [Adalogger overview](https://learn.adafruit.com/adafruit-adalogger-featherwing/overview) and [pinouts](https://learn.adafruit.com/adafruit-adalogger-featherwing/pinouts).

The SHT45 breakout has fixed I2C address `0x44`; the PCF8523 uses `0x68`, so both can share the Feather bus. Adafruit specifies typical SHT45 accuracy around ±0.1 °C and ±1% RH in its central operating range. Source: [SHT4x guide and pinouts](https://learn.adafruit.com/adafruit-sht40-temperature-humidity-sensor/pinouts).

## OTA constraint

The board has only 4 MB flash. The prototype partition table therefore uses two 1.875 MiB OTA slots and keeps logs/telemetry spool on microSD. The current linked image occupies about 1.29 MB of flash sections, leaving margin in either slot.

ESP-IDF documents that signed OTA verification without hardware Secure Boot uses the public key from the currently running signed image. Therefore the first image flashed must also be signed; an unsigned development image cannot establish the OTA trust chain. Rollback requires the new image to mark itself valid after bounded startup checks, otherwise ESP-IDF returns to the previous slot. Sources: [Secure Boot v2: signed app verification without hardware Secure Boot](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/security/secure-boot-v2.html), [OTA rollback](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-reference/system/ota.html), and [ESP HTTPS OTA](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/api-reference/system/esp_https_ota.html).

## Hardware-only checks

No device was connected or flashed while this note was written. The first physical session must verify GPIO10 SD CS, both I2C addresses, PSRAM mode, signed initial boot, MQTT/TLS heap margin, SD power-loss recovery, and A/B rollback before any production claim.
