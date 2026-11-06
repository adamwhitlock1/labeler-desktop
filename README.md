# labeler-desktop

A fast, modern desktop app (+ CLI) for printing text and images to a DYMO
LetraTag 200B over Bluetooth LE. Private project, not yet licensed for
distribution.

## Status

- `label-core` — pure protocol encoder (checksums, chunking, bit packing). Tested.
- `label-render` — text/image → 1-bit raster: dithering (threshold, Floyd–Steinberg,
  Atkinson), alignment, invert, contrast, pixel-exact preview PNGs. Tested.
- `label-device` — BLE discovery + connect + print via `btleplug`. **Verified against a
  real LT200B.**
- `label-cli` — `scan`, `text`, `image`, `preview` subcommands. Working end-to-end.
- `label-desktop` — GPUI Kit desktop app. In progress.

## Protocol credit

The wire protocol (BLE service/characteristic UUIDs, header/checksum format, chunking,
raster layout) was reverse engineered by the community, documented in
[sgilissen/LT200B-gui](https://github.com/sgilissen/LT200B-gui) and
[alexhorn/lt200b](https://github.com/alexhorn/lt200b) (both AGPL-3.0). This repo is an
independent reimplementation in Rust; no source code from either project was copied. If
this is ever distributed publicly, the AGPL-3.0 question for the protocol encoder needs a
real licensing review first — right now this project is unlicensed/private.

## Building

```bash
cargo build --workspace
cargo test --workspace
```

## macOS: Bluetooth permission

A plain `cargo run` binary has no `Info.plist`, so CoreBluetooth aborts without
`NSBluetoothAlwaysUsageDescription`. For CLI testing, wrap the binary in a minimal app
bundle first:

```bash
cargo build -p label-cli
./scripts/make_app_bundle.sh label-cli target/debug/label-cli LabelCLI
./dist/LabelCLI.app/Contents/MacOS/label-cli scan --seconds 8
```

The first run prompts for Bluetooth permission (grant it to whatever process launched the
binary — Terminal, if run from a shell).

## CLI usage

```bash
# find your printer's BLE id
label-cli scan --seconds 8

# print text
label-cli text --device <id> "Hello World" --size 48 --align center --dither floyd-steinberg

# print an image
label-cli image --device <id> photo.png --dither atkinson --invert

# preview without a printer
label-cli preview --text "Hello" --out preview.png --scale 8
```
