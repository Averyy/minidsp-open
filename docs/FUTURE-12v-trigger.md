# 12V Trigger (FUTURE — separate project, after v1)

ESP32 GPIO-controlled power switching for MiniDSP devices. Solves minidsp-rs #663 (no standby/power control).

## Why

MiniDSP devices have no standby control via USB/TCP. The Flex standby is IR-only. USB drops off the bus in standby. The 2x4 HD has no standby at all. The only way to power-cycle a MiniDSP remotely is to switch its DC power supply.

## Hardware

- ESP32 GPIO → IRLZ34N MOSFET (logic-level, N-channel) → switches MiniDSP 12V DC barrel jack power
- Flyback diode across the load (1N4007 or similar)
- ESP32 stays powered independently via its own USB — always reachable on WiFi even when MiniDSP is off
- Hardware cost: ~$0.50 (MOSFET + diode)

### Optional: Trigger INPUT
- Detect receiver/AVR power state via 12V trigger output → voltage divider → ESP32 GPIO input
- Auto-power MiniDSP when receiver turns on, auto-off when receiver turns off
- Useful for home theater setups where the AVR controls the power sequence

### Wiring

```
ESP32 GPIO pin ──┐
                 │
            ┌────┴────┐
            │  GATE   │
  MiniDSP   │         │
  DC GND ───┤ DRAIN   │  IRLZ34N
            │         │
  PSU GND ──┤ SOURCE  │
            └─────────┘
                 │
            ┌────┴────┐
            │ 1N4007  │  (flyback, cathode toward drain)
            └─────────┘
```

MiniDSP DC+ goes straight through. The MOSFET switches the ground side. When GPIO is HIGH, MOSFET conducts, MiniDSP powers on. When LOW, power is cut.

## API

| Method | Path | Description |
|--------|------|-------------|
| POST | `/trigger/on` | Power on (GPIO HIGH) |
| POST | `/trigger/off` | Power off (GPIO LOW) |
| GET | `/trigger/status` | Current state: `{"power": true/false, "input": true/false}` |

The `input` field reports the optional trigger input state (receiver power detection).

## CLI

```
minidsp trigger on
minidsp trigger off
minidsp trigger status
```

## Config

GPIO pin numbers configured in NVS (set during provisioning or via API):

```json
{
  "trigger_output_pin": 4,
  "trigger_input_pin": 5,
  "trigger_auto": false
}
```

`trigger_auto: true` enables automatic power-on/off based on the trigger input.

## Implementation

- ~50 lines of Rust: GPIO init, two HTTP handlers, one status read
- Pin config stored in NVS alongside WiFi credentials
- State persisted across reboots (NVS) — remembers last power state
- On boot: restore last power state from NVS

## Dependencies

- Requires the ESP32 core (phase 1) to be working: WiFi, HTTP API, NVS
- Requires hardware validation — need to verify MOSFET switching works reliably with MiniDSP's power draw
- The IRLZ34N is rated for 60V/20A — MiniDSP draws <1A at 12V, so massive margin

## Custom PCB (future)

ESP32-S3-WROOM module, USB-A male plug, LDO, MOSFET for trigger. USB stick form factor. BOM ~$5-8. Integrates trigger hardware directly on the board — no external wiring.

## Related

- minidsp-rs #663: "Feature request: standby/power control"
- minidsp-open.md: Device Behavior Notes (no standby control via USB/TCP)
