# minidsp-open

A controller for MiniDSP audio processors, in Rust. One codebase, two targets:

- **ESP32-S3 board:** plugs into the MiniDSP's USB port and puts it on your WiFi. It serves the [ezBEQ](https://github.com/3ll3d00d/ezbeq) web UI itself, so you can load bass EQ from your phone, and MiniDSP Device Console can connect to it like a WI-DG.
- **Linux, macOS, Windows:** a `minidsp` CLI and daemon that replaces [minidsp-rs](https://github.com/mrene/minidsp-rs). Same commands, HTTP and WebSocket API (port 5380) and TCP bridge (5333), so ezBEQ, Home Assistant and scripts keep working.

Downloads are on the [Releases page](https://github.com/Averyy/minidsp-open/releases/latest). Changes are in [CHANGELOG.md](CHANGELOG.md), open work in [PROGRESS.md](PROGRESS.md), and [llms.txt](llms.txt) is a compact API reference.

## Device support

Tested end to end on the **2x4 HD**. Every other model (Flex, Flex HT/HTx, Flex Eight, SHD, DDRC-24/88, 4x10/10x10 HD and the older SigmaStudio units) is modelled from MiniDSP's Device Console data but not yet confirmed on hardware. If you have one, a bug report with `minidsp probe` output helps.

## ESP32 board

You need an **ESP32-S3 N16R8** board (16 MB flash, 8 MB PSRAM; smaller ones don't have room). Bridge the two OTG pads on the back so the OTG port supplies 5 V, plug that port into the MiniDSP, and power the board from its other USB port. Details: [docs/ref-esp32-hardware.md](docs/ref-esp32-hardware.md).

First install, over USB, with the `-full.bin` from the latest release:

```bash
uvx --from esptool==5.6.0 esptool --chip esp32s3 write-flash 0x0 minidsp-open-esp32-X.Y.Z-full.bin
```

The board then starts a WiFi network called `miniDSP-Open-XXXXXX`. Join it and enter your WiFi details. Once it's connected it downloads the ezBEQ UI and catalogue, then serves them at `http://<board-ip>:5380` and as `minidsp-<serial>.local`. `/status` shows the board and MiniDSP, and has a rattle finder.

Later firmware updates go over WiFi from `http://<board-ip>:5380/update`, and a build that can't get back on the network rolls back by itself. A board moving from ESP-IDF 5.4 to 6.x needs one more USB flash. See [docs/ref-firmware-updates.md](docs/ref-firmware-updates.md).

## Desktop CLI and daemon

Download the archive for your platform and check it with `shasum -a 256 -c SHA256SUMS --ignore-missing`. The macOS binaries aren't signed; if macOS blocks `minidsp`, run `xattr -d com.apple.quarantine minidsp`.

```bash
minidsp                      # status
minidsp gain -- -20          # master volume
minidsp source usb
minidsp config 1             # preset 2 (presets count from 0)
minidsp -o jsonline          # status as JSON, as ezBEQ reads it
minidsp server               # HTTP/WebSocket API on :5380, TCP bridge on :5333
minidsp --tcp <board-ip> status   # talk to an ESP32 board or a WI-DG
```

Every compatible command, route and config key is listed in [docs/ref-compatibility.md](docs/ref-compatibility.md).

## Building

```bash
cargo build --release && cargo test   # desktop; Linux needs libudev-dev, libusb-1.0-0-dev, libasound2-dev
tools/esp-cargo build --release       # ESP32 firmware
```

The firmware needs the Xtensa Rust toolchain ([espup](https://github.com/esp-rs/espup)) and `ldproxy`; the build downloads ESP-IDF v6.0.3. [CONTRIBUTING.md](CONTRIBUTING.md) has the full setup.

## Security

There's no authentication by default: anything on your LAN can control the MiniDSP. The ESP32 has an optional password (set it on `/status`), and both targets have an optional maximum volume. Use a trusted network. See [SECURITY.md](SECURITY.md).

## Credits

- [minidsp-rs](https://github.com/mrene/minidsp-rs) by mrene worked out most of the MiniDSP USB protocol. minidsp-open is a separate implementation that builds on that work.
- [ezBEQ](https://github.com/3ll3d00d/ezbeq) and the [BEQ catalogue](https://github.com/3ll3d00d/beqcatalogue) by 3ll3d00d and the BEQ community. The board downloads them at runtime; they aren't bundled here.
- Device definitions and factory defaults come from MiniDSP's Device Console; [NOTICE](NOTICE) covers what's included and on what terms.

minidsp-open isn't affiliated with or endorsed by MiniDSP.

## License

MIT, except the third-party files listed in [NOTICE](NOTICE).
