#!/usr/bin/env bash
set -euo pipefail
umask 077

key="${1:-}"
bundle="${2:-}"
out_dir="${3:-.prototype-secrets/ota}"
elf="target/xtensa-esp32s3-espidf/release/esp32-environmental"

[[ -n "$key" && -n "$bundle" ]] || {
  echo "usage: $0 <signing-key> <device-bundle.json> [output-directory]" >&2; exit 1;
}
for secret_path in "$key" "$bundle" "$out_dir"; do
  [[ "$secret_path" == .prototype-secrets/* || "$secret_path" == "$PWD/.prototype-secrets/"* ]] || {
    echo "keys, bundle, and output must remain beneath .prototype-secrets" >&2; exit 1;
  }
done
[[ -f "$key" ]] || { echo "missing signing key: $key" >&2; exit 1; }
[[ -f "$bundle" ]] || { echo "missing device bundle: $bundle" >&2; exit 1; }
mkdir -p "$out_dir"
source "$HOME/export-esp.sh"
export MOTHERSHIP_DEVICE_BUNDLE="$(cd "$(dirname "$bundle")" && pwd)/$(basename "$bundle")"
cargo build --release
espflash save-image --chip esp32s3 --flash-size 4mb "$elf" "$out_dir/environmental-unsigned.bin"
espsecure sign-data --version 2 --keyfile "$key" --output "$out_dir/environmental-signed.bin" "$out_dir/environmental-unsigned.bin"
size="$(wc -c < "$out_dir/environmental-signed.bin")"
(( size <= 0x1E0000 )) || { echo "signed image exceeds 1.875 MiB OTA slot" >&2; exit 1; }
idf_build="$(ls -dt target/xtensa-esp32s3-espidf/release/build/esp-idf-sys-*/out/build | head -1)"
cp "$idf_build/bootloader/bootloader.bin" "$out_dir/bootloader.bin"
cp "$idf_build/partition_table/partition-table.bin" "$out_dir/partition-table.bin"
cp "$idf_build/ota_data_initial.bin" "$out_dir/ota-data-initial.bin"
(
  cd "$out_dir"
  shasum -a 256 bootloader.bin partition-table.bin ota-data-initial.bin environmental-signed.bin \
    > flash-bundle.sha256
)
chmod 600 "$out_dir"/*
echo "signed device-specific application written beneath $out_dir"
