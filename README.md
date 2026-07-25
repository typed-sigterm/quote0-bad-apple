# quote0-bad-apple ![Last commit](https://img.shields.io/github/last-commit/typed-sigterm/quote0-bad-apple/main) ![License](https://img.shields.io/github/license/typed-sigterm/quote0-bad-apple) ![OSS Lifecycle](https://img.shields.io/osslifecycle?file_url=https%3A%2F%2Fraw.githubusercontent.com%2Ftyped-sigterm%2Fquote0-bad-apple%2Fmain%2FOSSMETADATA) [![GitHub Stars](https://img.shields.io/github/stars/typed-sigterm/quote0-bad-apple)](https://github.com/typed-sigterm/quote0-bad-apple)

Play [Bad Apple!!](https://www.nicovideo.jp/watch/sm8628149) on [MindReset Quote/0](https://dot.mindreset.tech/docs/quote_0) e-ink display.

👉 Checkout [demo](https://www.bilibili.com/video/BV1wJ3g68E1Q).

## Requirements

- [MindReset Quote/0](https://dot.mindreset.tech/docs/quote_0), or any ESP32-C3 + UC8251D 152×296 panel
- USB-C cable
- [Rust](https://rust-lang.org/tools/install/)
- [espflash (v4)](https://crates.io/crates/espflash)
- [ffmpeg (v8)](https://ffmpeg.org) (in PATH)
- 2.4 GHz WiFi

> [!CAUTION]
> flashing replaces the stock Quote/0 firmware (Dot API, cloud features, ...). Keep a way to restore it if you care about that.

## Guide

### 1. Flash the firmware

Clone the repo. Set your WiFi credentials as environment variables in [`firmware/.cargo/config.toml`](firmware/.cargo/config.toml):

```toml
[env]
WIFI_SSID = "your-ssid"
WIFI_PASSWORD = "your-password"
```

> [!WARNING]
> This file is tracked by git, so add them to `.git/info/exclude` or set in your shell instead; they take at build time.

Plug in the Quote/0 via USB-C, then:

```sh
cd firmware
cargo run --release
```

On boot the panel shows a checkerboard (proof the display driver works), then the serial log prints the WiFi connection and the IP it got via DHCP.

### 2. Stream a video

```sh
cd host
cargo run --release -- /path/to/bad-apple.mp4 --loop
```

That's it:

```
 INFO quote0-bad-apple-host v0.0.0
 INFO video    : /path/to/bad-apple.mp4
 INFO interval : 1.00s/frame (sampling 1.000 fps)
 INFO transform: rotate=Ccw  invert=false
 INFO frames   : ~219 (progress + ETA enabled)
 INFO host     : 192.168.0.103:8899  (tcp :8899, discover udp :18888)
 INFO stream: waiting for device to connect...
 INFO stream: checklist: device & this PC on the SAME WiFi, firmware flashed with your WiFi credentials, and this app allowed through the firewall
[########----------------]  73/219  33%  0.9/s  eta 2m
```

The host broadcasts its address on the LAN (UDP :18888), the device finds it, connects back (TCP :8899). Check the firewall if it fails.

### Options

Run `cargo run --release -- --help` in `host` directory to view options.

## Loopback test without hardware

Two terminals, no device needed:

```sh
cargo run -p quote0-bad-apple-host -- --mock-device --refresh 0.5
cargo run -p quote0-bad-apple-host -- video.mp4 --interval 0.5
```

The mock device performs discovery, receives frames, and acks like real hardware.
Tip: if a real flashed Quote/0 is powered on nearby it will also answer the
broadcast and "steal" the stream — pass `--discover-port 18889 --port 8898` to
both commands to keep the test local.
