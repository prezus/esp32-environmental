#!/usr/bin/env bash
set -euo pipefail
umask 077

key="${1:-.prototype-secrets/ota-signing-key.pem}"
out_dir="${2:-.prototype-secrets/ota}"
elf="target/xtensa-esp32s3-espidf/release/esp32-environmental"

[[ "$key" == .prototype-secrets/* || "$key" == "$PWD/.prototype-secrets/"* ]] || {
  echo "signing key must remain beneath .prototype-secrets" >&2; exit 1;
}
[[ -f "$key" ]] || { echo "missing signing key: $key" >&2; exit 1; }
mkdir -p "$out_dir"
source "$HOME/export-esp.sh"
cargo build --release
espflash save-image --chip esp32s3 --flash-size 4mb "$elf" "$out_dir/environmental-unsigned.bin"
espsecure sign-data --version 2 --keyfile "$key" --output "$out_dir/environmental-signed.bin" "$out_dir/environmental-unsigned.bin"
size="$(wc -c < "$out_dir/environmental-signed.bin")"
(( size <= 0x1E0000 )) || { echo "signed image exceeds 1.875 MiB OTA slot" >&2; exit 1; }
shasum -a 256 "$out_dir/environmental-signed.bin" > "$out_dir/environmental-signed.bin.sha256"
chmod 600 "$out_dir"/*
echo "signed OTA application written beneath $out_dir"
