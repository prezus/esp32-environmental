# ESP32-S3 Environmental Logger — build & device tasks.
# Run `just` with no args to list recipes.

# Sourcing the esp toolchain env (PATH to xtensa tools + LIBCLANG_PATH for bindgen).
export-esp := "source $HOME/export-esp.sh"

# Show available recipes.
default:
    @just --list

# Compile (release).
build:
    {{export-esp}} && cargo build --release

# Compile, flash, and open the serial monitor.
flash:
    {{export-esp}} && cargo run --release

# Alias for `flash`.
run: flash

# Open the serial monitor on the connected device.
monitor:
    {{export-esp}} && espflash monitor

# Check formatting and lints.
fmt:
    {{export-esp}} && cargo fmt

clippy:
    {{export-esp}} && cargo clippy --release -- -D warnings

test:
    cargo +stable test -p environmental-core --target aarch64-apple-darwin

ci:
    {{export-esp}} && cargo fmt --all -- --check
    {{export-esp}} && cargo clippy --release -- -D warnings
    cargo +stable test -p environmental-core --target aarch64-apple-darwin

# Remove build artifacts.
clean:
    cargo clean
