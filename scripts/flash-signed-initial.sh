#!/usr/bin/env bash
set -euo pipefail

port="${1:-}"
bundle="${2:-}"
confirmation="${3:-}"

[[ -n "$port" && -n "$bundle" ]] || {
  echo "usage: $0 <serial-port> <image-directory> --erase-and-flash" >&2; exit 1;
}
[[ "$confirmation" == "--erase-and-flash" ]] || {
  echo "refusing to modify hardware without --erase-and-flash" >&2; exit 1;
}
[[ "$port" != "/dev/cu.usbserial-110" ]] || {
  echo "refusing forbidden serial port $port" >&2; exit 1;
}
[[ "$bundle" == .prototype-secrets/* || "$bundle" == "$PWD/.prototype-secrets/"* ]] || {
  echo "image directory must remain beneath .prototype-secrets" >&2; exit 1;
}
for file in bootloader.bin partition-table.bin ota-data-initial.bin environmental-signed.bin flash-bundle.sha256; do
  [[ -f "$bundle/$file" ]] || { echo "missing $bundle/$file" >&2; exit 1; }
done
(
  cd "$bundle"
  shasum -a 256 -c flash-bundle.sha256
)
source "$HOME/export-esp.sh"
espflash board-info --port "$port"
esptool --chip esp32s3 --port "$port" erase-flash
esptool --chip esp32s3 --port "$port" --baud 460800 write-flash \
  --flash-mode dio --flash-freq 80m --flash-size 4MB \
  0x0 "$bundle/bootloader.bin" \
  0x8000 "$bundle/partition-table.bin" \
  0xf000 "$bundle/ota-data-initial.bin" \
  0x20000 "$bundle/environmental-signed.bin"
echo "signed initial image flashed to $port"
