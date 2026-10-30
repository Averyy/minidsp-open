# minidsp-open

An open-source controller for MiniDSP audio processors, written in Rust. It runs two ways from one codebase:

- **On a cheap ESP32-S3 dev board** that plugs into the MiniDSP's USB port and puts it on your WiFi. The board hosts the [ezBEQ](https://github.com/3ll3d00d/ezbeq) web UI itself, so you can load bass EQ for a movie from your phone with no computer, Raspberry Pi, or Python install. It also stands in for the WI-DG: MiniDSP Device Console can connect to it over the network.
- **On Linux, macOS, or Windows** as a `minidsp` CLI and daemon that's a drop-in replacement for [minidsp-rs](https://github.com/mrene/minidsp-rs). Same commands, same HTTP and WebSocket API on port 5380, same TCP bridge on 5333, so ezBEQ, Home Assistant integrations, and scripts keep working unchanged.

## Status

There aren't any tagged releases yet; build from source for now. The download
steps below describe the planned release workflow. See [CHANGELOG.md](CHANGELOG.md)
for changes and [PROGRESS.md](PROGRESS.md) for current status and open work.

Hardware-tested end to end on the **MiniDSP 2x4 HD**: volume, mute, source, presets, PEQ/crossover/routing writes, config read-back, snapshot backup and restore, factory reset, FIR loading, ezBEQ, the Device Console bridge, measurement sweeps over USB audio from both the desktop daemon and the ESP32, and rattle-finder tones from the ESP32.

Every other device is modelled from MiniDSP's own Device Console data, which covers all 13 current hardware/DSP combinations (Flex, Flex HT/HTx, Flex Eight, SHD, DDRC-24/88, 4x10/10x10 HD and more) plus the older SigmaStudio-based units. Those definitions haven't been confirmed on real hardware yet. If you own one, a bug report with `minidsp probe` output helps a lot.

## ESP32 board

### Hardware

- An **ESP32-S3 N16R8** dev board (16 MB flash, 8 MB PSRAM). Smaller variants don't have room for USB audio, WiFi, and the ezBEQ catalogue at once.
- Bridge the two OTG solder pads on the back of the board so its OTG port supplies 5 V. Without that the MiniDSP won't enumerate.
- Plug the board's OTG port into the MiniDSP's USB port, and power the board from its other USB port. The MiniDSP keeps its own power supply.

See `docs/ref-esp32-hardware.md` for details.

### First install

Download `minidsp-open-esp32-X.Y.Z-full.bin` from the [latest release](https://github.com/Averyy/minidsp-open/releases/latest) and flash it over USB:

```bash
uvx --from esptool==5.6.0 esptool --chip esp32s3 write-flash 0x0 minidsp-open-esp32-X.Y.Z-full.bin
```

On first boot the board starts a WiFi network called `miniDSP-Open-XXXXXX`. Join it, and a setup page asks for your WiFi name and password. After it connects, it downloads the ezBEQ UI and catalogue (a few minutes) and then serves them at `http://<board-ip>:5380`. It advertises itself over mDNS as `minidsp-<serial>.local`.

`http://<board-ip>:5380/status` shows the board and MiniDSP status. It also has a rattle finder: it plays a list of steady tones at volumes you pick and shows which tone is playing, so you can walk the room and find what buzzes.

### Updates

After that first USB flash, firmware updates go over WiFi. Boards moving from ESP-IDF 5.4 to 6.x need another USB flash to update the bootloader; later 6.x builds can use OTA. The board checks GitHub for new releases daily, and `http://<board-ip>:5380/update` shows the current version, installs a release, or takes an uploaded `.bin`. A new build that can't get back on the network rolls back to the previous one on its own. The ezBEQ catalogue updates itself whenever new titles are published. More in `docs/ref-firmware-updates.md`.

## Desktop CLI and daemon

Download the archive for your platform from the [latest release](https://github.com/Averyy/minidsp-open/releases/latest), or build it (see below). It uses minidsp-rs's command syntax:

```bash
minidsp                      # status
minidsp gain -- -20          # master volume
minidsp source usb
minidsp config 1             # switch to preset 2
minidsp input 0 peq 0 set -- 1.0 0.0 0.0 0.0 0.0
minidsp -o jsonline          # machine-readable status, as ezBEQ reads it
minidsp server               # HTTP/WebSocket API on :5380, TCP bridge on :5333
```

`minidsp --tcp <board-ip>:5333 status` talks to an ESP32 board (or a WI-DG) instead of a local USB device. `minidsp --daemon-url http://<board-ip>:5380 rattle --tones 30,40,50 --volume=-40` runs the rattle finder against a board or a desktop daemon and prints each tone as it starts. `docs/ref-compatibility.md` lists every command, route, and config key that's kept compatible with minidsp-rs.

## Building

Desktop (stable Rust; on Linux, install `libudev-dev`, `libusb-1.0-0-dev`, and `libasound2-dev` first):

```bash
cargo build --release
cargo test
```

The ESP32 firmware needs the Xtensa Rust toolchain ([espup](https://github.com/esp-rs/espup)), ESP-IDF v6.0.3 (esp-idf-sys downloads it), and `ldproxy`. The USB audio driver comes from the ESP Component Registry as `averyy/usb_host_uac2`, pinned to `0.1.3`; no sibling checkout is needed. [CONTRIBUTING.md](CONTRIBUTING.md#esp32-setup) has the full setup and local driver override details.

```bash
tools/esp-cargo build --release
```

For driver development, use `ESP_UAC2_HOST_PATH="$HOME/code/esp-uac2-host/components/usb_host_uac2" tools/esp-cargo build --release` to build against a local working copy without editing tracked files.

## Security

An optional maximum master volume limits HTTP, WebSocket, TCP bridge, and rattle playback levels; configure it through `/settings` (or the ESP32 `/status` page). It is off by default.

Use minidsp-open on a trusted LAN. See [SECURITY.md](SECURITY.md) for the trust model, limits, and private vulnerability reporting.

## Credits

- [minidsp-rs](https://github.com/mrene/minidsp-rs) by mrene worked out most of the MiniDSP USB protocol. This project is a separate implementation, but it wouldn't exist without that work.
- [ezBEQ](https://github.com/3ll3d00d/ezbeq) and the [BEQ catalogue](https://github.com/3ll3d00d/beqcatalogue) by 3ll3d00d and the BEQ community. The ESP32 serves the ezBEQ web UI and catalogue as published; they're downloaded at runtime, not bundled in this repo.
- Device definitions and factory defaults come from MiniDSP's Device Console. See NOTICE for what's included and on what terms.

minidsp-open isn't affiliated with or endorsed by MiniDSP.

## License

MIT, except the third-party files listed in NOTICE.
