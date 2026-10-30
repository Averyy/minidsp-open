# ESP32-S3 Hardware Reference

## Dev Board

ESP32-S3 N16R8 (16 MB flash, 8 MB PSRAM): https://www.aliexpress.com/item/1005008752956562.html

Select the **N16R8** variant. The N8R2 (2 MB PSRAM) is too constrained for USB audio + WiFi + HID simultaneously.

## Enable USB OTG Mode

On most dev boards (this one included) there's 2 solder pads on the back of the ESP32 that need to be bridged together so that the device will supply 5v on the OTG (left) USB port. In order for the device to connect to the ESP32 this must be done.

## Why N16R8

- 8 MB PSRAM: room for USB audio ring buffers, WiFi stack, HID control, HTTP server
- 16 MB flash: room for firmware + any future features
- Native USB OTG: USB host mode for miniDSP HID control + audio output
- Full Speed USB (12 Mbps): enough bandwidth for 48kHz/24-bit/stereo (2.3 Mbps)

## Key Findings

- **XMOS AUDIO_CLASS_FALLBACK is disabled by default** in the XMOS reference firmware. miniDSP almost certainly did not enable it (no PS4/PS5 support, Windows driver named "UAC2", no changelog mentions). The miniDSP will likely present UAC2-only descriptors at Full Speed.
- **USB audio output uses `averyy/usb_host_uac2` from the ESP Component Registry**, pinned in `app-esp32/Cargo.toml`. A local esp-uac2-host checkout is only needed for driver or simulator development; see [CONTRIBUTING.md](../CONTRIBUTING.md#usb-audio-driver-development).
- If by some chance UAC1 fallback IS present, Espressif's existing `usb_host_uac` v1.3.3 handles everything.
