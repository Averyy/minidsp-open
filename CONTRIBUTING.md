# Contributing

Bug reports, device reports, and pull requests are all welcome.

## Reporting a device

Most of the device support is modelled from MiniDSP's own data and hasn't met real hardware. If you have a MiniDSP other than the 2x4 HD, the most useful thing you can send is an issue with:

- `minidsp probe` output (it prints the hardware ID, DSP version, and firmware the device reports)
- what you tried, what you expected, and what happened
- `minidsp -o json` status output if the device talks at all

Don't paste your serial number or LAN addresses if you'd rather not; they aren't needed.

## Layout

| Path | What |
|------|------|
| `protocol/` | Wire protocol, commands, device tables. Shared by both targets, no platform code. |
| `minidsp/` | Async device client: transports (USB HID, TCP, WebSocket), command queue, config shadow, snapshots. |
| `daemon/` | Desktop HTTP/WebSocket API, TCP bridge, UDP discovery. |
| `app-desktop/` | The `minidsp` CLI binary. |
| `app-esp32/` | ESP32-S3 firmware (blocking std on ESP-IDF). Not part of the Cargo workspace. |
| `tools/` | Flashing, OTA push, catalogue bundle builder, live validation harness. |
| `docs/` | `ref-*.md` are stable references; `TODO*.md` are open work. |

[PROJECT.md](PROJECT.md) covers the architecture and [docs/ref-compatibility.md](docs/ref-compatibility.md) the external contract. [PROGRESS.md](PROGRESS.md) tracks current status and open work; [CHANGELOG.md](CHANGELOG.md) records changes.

## Ground rules

- **Don't break existing clients.** The CLI, HTTP API, WebSocket, TCP bridge, and UDP discovery must stay compatible with minidsp-rs and with ezBEQ as `docs/ref-compatibility.md` describes. New behaviour is additive. If a change touches one of those surfaces, say so in the PR.
- **MiniDSP's XML is the source of truth for addresses.** SHARC devices use Device Console's `DSP_default_parameters.xml`; legacy devices use the SigmaStudio `config.xml` exports. Don't hand-tune addresses that disagree with them.
- **Write original code.** Protocol knowledge from minidsp-rs is credited, but don't copy its code.
- **Fix root causes.** A retry or sleep that hides a bug isn't a fix.
- Keep `cargo test` green and add a regression test with bug fixes where the logic is testable on the host.

## Desktop setup

Stable Rust. On Linux, install `libudev-dev`, `libusb-1.0-0-dev`, `libasound2-dev`, and `pkg-config`.

```bash
cargo build
cargo test
cargo run --bin minidsp -- probe
```

## ESP32 setup

1. Install Python 3.10 or newer (with venv support), CMake 3.22.1 or newer, Ninja, and libclang. These are the [ESP-IDF 6.0 tool requirements](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s3/migration-guides/release-6.x/6.0/tools.html). On Ubuntu, install `build-essential pkg-config libssl-dev libudev-dev cmake ninja-build python3 python3-venv libclang-dev`; check `python3 --version` and `cmake --version` on older distributions.
2. Install the Xtensa toolchain with [espup](https://github.com/esp-rs/espup) (`espup install --targets esp32s3 --std --toolchain-version 1.93.0.0`), then `cargo install ldproxy`. Use that toolchain version: CI pins it, and Xtensa Rust 1.97.0.0 crashes LLVM while compiling this firmware. `tools/esp-cargo` sources `~/export-esp.sh` (override with `ESP_EXPORT_SCRIPT` if needed). For an existing installation, rerun the same `espup install` command; `espup update` moves to the newest toolchain. The `--std` option leaves installation of IDF's matching Xtensa GCC to esp-idf-sys.
3. Build. esp-idf-sys downloads ESP-IDF v6.0.3, selected in `app-esp32/.cargo/config.toml`, and its matching build tools on the first build, which takes a while. It also downloads `averyy/usb_host_uac2` exactly `0.1.3` from the ESP Component Registry, alongside the other managed components. Their resolved versions are tracked in `app-esp32/components_esp32s3.lock`. No separate ESP-IDF or driver checkout is needed.

```bash
tools/esp-cargo build            # size-optimised debug build, fits a 3 MB OTA slot
tools/esp-cargo build --release
```

Do not build with an activated ESP-IDF environment (`. export.sh`): its environment variables override the pinned version. The repo wrapper clears those variables and leaves IDF installation to esp-idf-sys. `tools/esptool` uses `uvx` to run the matching esptool 5.6.0; install [uv](https://docs.astral.sh/uv/getting-started/installation/) for flashing and the Python tools.

### USB audio driver development

To use a working copy of [esp-uac2-host](https://github.com/Averyy/esp-uac2-host), pass its component directory to the wrapper (an absolute path is recommended):

```bash
ESP_UAC2_HOST_PATH="$HOME/code/esp-uac2-host/components/usb_host_uac2" tools/esp-cargo build --release
```

The wrapper uses esp-idf-sys 0.38.1's `ESP_IDF_SYS_EXTRA_COMPONENTS_FILE` hook to generate an ignored TOML file under `app-esp32/target/`. It preserves the other components and binding headers, replacing only the driver with `component_dirs`. In both modes, the generated filename hashes the component declarations, lockfile, and wrapper so Cargo reruns esp-idf-sys when those change. The component manager supports [`override_path`](https://docs.espressif.com/projects/idf-component-manager/en/latest/reference/manifest_file.html#override-path-registry-override), but esp-idf-sys does not expose it. The wrapper restores the registry lockfile after local builds, including failed builds. Avoid simultaneous registry and local builds, which share that lockfile.

In override mode, `ESP_IDF_GLOB_UAC2_*` tracks the driver's C sources, headers, CMakeLists.txt, Kconfig, and manifest because esp-idf-sys 0.38 does not watch `component_dirs`. Keep the globs in `tools/esp-cargo` in step with the driver's build inputs. Run the command without `ESP_UAC2_HOST_PATH` to return to the registry package. Registry updates require changing the exact version in `app-esp32/Cargo.toml`, rebuilding, and committing the updated component lockfile. Keep Cargo target directories directly under `app-esp32/` (`target/` or `target-heaptrace/`): esp-idf-sys resolves project paths from their parent.

### Flashing and testing on hardware

- `tools/flash-esp32.sh --port <serial-port>` builds the firmware and the ezBEQ bundle, then flashes both over USB. You need this the first time, for the first move from ESP-IDF 5.4 to 6.x, and whenever the partition table or bootloader changes. Later 6.x builds can use OTA.
- `tools/ota-push.sh --board http://<board-ip>:5380` builds and installs over WiFi. A build that can't reach the network rolls back on its own.
- `tools/run-live-validation.py` exercises the HTTP/WebSocket/TCP/CLI surfaces against a desktop daemon or a board. `--allow-destructive` restores a snapshot afterwards; `--allow-audio` plays test sweeps.

`docs/ref-esp32-boot.md`, `docs/ref-esp32-logging.md`, and `docs/ref-firmware-updates.md` cover the boot sequence, debug endpoints, and OTA.

## Pull requests

- One topic per PR, with a description of what changed and how you tested it (host tests, which hardware, which client).
- CI runs `cargo test` and an ESP32 release build. Run them locally first.
- Run `cargo fmt` on the files you touch.
- Note anything you couldn't verify on hardware.
- Record user-visible changes under Unreleased in [CHANGELOG.md](CHANGELOG.md), and keep [PROGRESS.md](PROGRESS.md) limited to current status and open items.

## Releases

Maintainers tag `vX.Y.Z` after bumping the versions in the root `Cargo.toml` and `app-esp32/Cargo.toml`. CI builds the firmware and desktop binaries and publishes the GitHub release that boards update from. The steps are in `docs/ref-firmware-updates.md`.
