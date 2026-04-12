# ESP32-S3 RGB LED Reference

Reference note for the onboard RGB LED on the ESP32-S3 dev boards used by `app-esp32`.

This is intentionally board-family-specific. Cheap ESP32-S3 N16R8/N8R2 boards have multiple near-identical revisions and clones, so "ESP32-S3" alone is not enough to identify the LED wiring.

## Short Answer

- The large onboard LED on the common dual-USB ESP32-S3 N16R8 board family used here is a single addressable RGB LED, not a plain analog RGB LED.
- The common wiring for this board family is `GPIO48 -> WS2812`-class LED (`NeoPixel` style).
- Treat it as:
  - pin: `GPIO48`
  - pixel count: `1`
  - protocol: `WS2812` / addressable RGB
  - verified color order on the tested board: `GRB`
- The boards currently on hand light the LED during initial power-up, so the LED is definitely populated and connected on the hardware currently being used.

## What This Means In Practice

The LED should be driven like a one-pixel LED strip, not like a normal `gpio_set_level()` LED.

That distinction matters:

- A plain LED can be toggled with a simple high/low GPIO write.
- A WS2812-class LED requires a tightly timed serial waveform.
- Board/vendor wording such as "not directly lit through GPIO" is best interpreted as "the LED data line is routed to GPIO48, but the LED itself expects WS2812 timing, not a raw HIGH/LOW output."

## Why `GPIO48` Is The Right Default

The strongest board-specific source is the VCC-GND `YD-ESP32-S3` repo for this board family. It documents:

- a `WS2812` onboard RGB LED
- the LED being driven by `GPIO48`
- `GPIO48` in the pin table explicitly labeled as the RGB LED pin

That matches:

- reseller/product pages for the same board family that advertise an onboard `WS2812 LED`
- third-party manual pages that describe the same dual-USB N16R8 family and call out a `RGB-LED WS2812`
- multiple community reports using `GPIO48` successfully on boards in this family

## Known Working Configuration For This Repo

For `minidsp-open`, the currently known-good configuration is:

- hardware family: dual-USB ESP32-S3 N16R8/N8R2 dev boards similar to YD/UICPAL/VCC-GND style boards
- onboard LED type: one addressable RGB pixel
- verified working data pin: `GPIO48`
- verified working color order: `GRB`

Live verification on 2026-04-12 on the connected dev board (`USB Single Serial`, serial `led-test-board`) showed the expected `red -> green -> blue -> amber -> white -> off` sequence using the RMT driver in `app-esp32/src/led.rs`.

Later on the same board, the product-state demo was also tuned and visually checked:

- state-demo entrypoint: `app-esp32/src/bin/led-demo.rs`
- demo helper module: `app-esp32/src/led_demo.rs`
- state hold in the current demo: `10 s`
- breathing period in the current demo: `4.5 s`
- breathing endpoint dwell in the current demo: about `180 ms` at both fully off and fully on
- breathing floor: full fade to black (`0.0`)
- low-brightness hue fix: non-black steps preserve the source channel mix so amber does not collapse into red near the bottom of the fade

The next tuning pass changed the unavailable-state amber behavior:

- `MiniDspUnavailable` no longer breathes all the way to black
- it now breathes between a low amber floor and full amber so the low end still reads as amber
- the current checked-in `led-demo` entrypoint isolates that amber-only breathe loop for visual tuning

Production integration landed later on 2026-04-12:

- the real firmware now owns the LED through a dedicated controller thread in normal task context
- `app-esp32/src/main.rs`, `app-esp32/src/wifi.rs`, and `app-esp32/src/updater.rs` now feed coarse state changes into that controller
- live post-flash board checks on the same `led-test-board` dev board confirmed the production blue and amber states were driving correctly

If a later board revision proves otherwise, update this note with the exact board photo, silk labels, and working pin/order.

## Important Caveats About Clones And Revisions

These boards are inconsistent in the market.

Known caveats:

- Some clone boards ship with the tiny `RGB` solder jumper open, which physically disconnects the LED data path.
- Some reports mention `GPIO47` or `GPIO38` on other revisions or mislabeled clones.
- Some boards have the LED wired correctly but the color order is swapped.
- The same seller listing may ship different PCB revisions over time.

For the hardware currently used in this repo, the solder-jumper caveat is less important because the boards already light the LED on initial power-up. That is strong evidence that the LED is present and the data path is already connected.

Still, if a future board does not respond:

1. Inspect the `RGB` solder jumper near the LED.
2. Reconfirm the exact PCB revision and silk labels.
3. Only then try alternative pins such as `GPIO47` or `GPIO38`.

Do not start by assuming the firmware is wrong.

## Related Board LEDs

On this board family the big RGB LED is not the only LED present.

- `PWR LED`: power indicator only, not software-controlled
- `TX LED`: activity LED on the UART TX line, commonly tied to `GPIO43`
- `RX LED`: activity LED on the UART RX line, commonly tied to `GPIO44`

Those are separate from the addressable RGB LED and should not be confused with it.

## Repo State Today

Current `app-esp32` state:

- there is now a small RMT-based LED driver in `app-esp32/src/led.rs`
- there is a standalone LED demo binary at `app-esp32/src/bin/led-demo.rs`
- demo-only tuning and bring-up loops now live in `app-esp32/src/led_demo.rs`
- the raw color verification loop is still available there for pin/order bring-up
- the product-state demo is still available there for reviewing all status colors/timing
- the current checked-in `led-demo` entrypoint loops the amber-only breathe test while `MiniDspUnavailable` is being tuned
- the production firmware now has a real LED status controller wired into `app-esp32/src/main.rs`
- that production path also consumes WiFi reconnect/provisioning signals from `app-esp32/src/wifi.rs`
- real ezBEQ download/write activity now drives the update state from `app-esp32/src/updater.rs`

So the LED hardware, GPIO selection, `GRB` ordering, the state-demo timing/brightness choices, the amber-floor workaround, and the production coarse-state plumbing are now all in place. The demo binary remains the tuning/bring-up tool; the real firmware now uses the same shared WS2812 write path and state-rendering behavior for normal runtime status.

## Recommended Driver Strategy

Use the current direct `esp-idf-hal` RMT implementation in `app-esp32/src/led.rs`, not Arduino helpers and not ad hoc bit-banging.

Why:

- The current one-pixel board LED does not need an extra strip component layer.
- The checked-in code already drives the board successfully with `esp-idf-hal` RMT primitives.
- The production integration now uses that same path, so keeping one driver avoids a second abstraction split.
- This repo is Rust + ESP-IDF, so Arduino-specific hidden helpers such as `RGB_BUILTIN` are the wrong abstraction.

## Recommended Backend

Use the `RMT` backend for the onboard LED.

Notes:

- For a single onboard pixel, RMT is simple and cheap.
- On ESP32-S3, Espressif documents that RMT DMA support exists and specifically recommends S3 for RMT workloads when WiFi/Bluetooth are active.
- For `1` LED, DMA is probably not required. `with_dma = false` should be sufficient and simpler.
- If the implementation ever expands to more LEDs or more complex animations, reconsider DMA.

## Important Constraint

Do not drive the LED from interrupt context.

Espressif explicitly documents that `rmt_transmit()` must be called from task context, not from an ISR. That means:

- no direct LED writes inside ISR callbacks
- no LED updates inside low-level USB/interrupt code
- centralize LED updates in a normal task/thread/module

## Recommended Implementation Shape In This Repo

The production implementation now follows this shape:

1. Keep the dedicated `app-esp32/src/led.rs` module as the production driver base.
2. Use the small `LedState` / `LedMode` model for user-visible coarse states.
3. Give one controller thread exclusive ownership of the LED handle.
4. Drive animations from that normal Rust thread.
5. Only send state changes or pattern requests into that module; do not let random code paths write the LED directly.

This keeps the LED from becoming another cross-cutting side effect.

## Suggested `LedState` Model

Suggested high-level states for `minidsp-open`:

- `Booting`
- `ProvisioningAp`
- `WifiConnecting`
- `MiniDspUnavailable`
- `MiniDspReady`
- `BackgroundUpdate`
- `Fatal`

This is intentionally minimal. A one-pixel status LED should answer a coarse question, not encode lots of internal software nuance.

## Recommended Status Mapping

Final recommended color/pattern mapping:

- `Booting`: solid amber
- `ProvisioningAp`: slow blue breathe
- `WifiConnecting`: quick blue flash
- `MiniDspUnavailable`: slow amber breathe
- `MiniDspReady`: dim solid green
- `BackgroundUpdate`: slow green breathe
- `Fatal`: solid red

Current tuned demo behavior on the tested board:

- solid/flash states use slightly brighter peak values than the first bring-up pass, but remain intentionally low overall for rack use
- blue and green breathing states fade all the way to black
- breathing states include a very short dwell at both fully off and fully on, while amber unavailable breathing uses the `AMBER_MIN` floor instead of full black
- amber's low-end fade keeps its hue instead of falling through red on the last visible step

Current unavailable-state amber tuning:

- `MiniDspUnavailable` now uses an amber-specific low endpoint instead of black
- that amber floor is intentionally just high enough to keep the low end looking amber on this board
- blue and green breathing states can still use black as the low endpoint if that reads better

Current checked-in color constants:

- `RED = (28, 0, 0)`
- `GREEN = (0, 24, 0)`
- `BLUE = (0, 0, 28)`
- `AMBER = (30, 13, 0)`
- `AMBER_MIN = (4, 2, 0)`
- `WHITE = (20, 20, 20)`

State meaning:

- `ProvisioningAp` means no WiFi credentials are available and the captive portal is the next required user action.
- `WifiConnecting` means the device is trying to get onto the network.
- `MiniDspUnavailable` intentionally covers both "waiting for first device" and "device path down and retrying". The LED does not try to distinguish those two cases.
- `MiniDspReady` means the box is operational for its core job.
- `BackgroundUpdate` means real ezBEQ catalogue update work is happening, i.e. real download/write activity rather than a trivial no-op check.
- `Fatal` is reserved for latched failures where the device cannot perform its core role and is not in a meaningful automatic recovery path.

Design rules:

- Prefer dim output. These onboard LEDs are very bright at full scale.
- Keep the state machine simple and readable from across the room.
- Avoid flashing on every command or packet; that gets annoying fast.
- Do not add extra "warning" or "error" states unless they materially change what the user should do next.
- Use rhythm more than color changes if later boards have odd color ordering.

## Priority Rules

Recommended priority order:

1. `Fatal`
2. `ProvisioningAp`
3. `WifiConnecting`
4. `MiniDspUnavailable`
5. `BackgroundUpdate`
6. `MiniDspReady`
7. `Booting`

Override policy:

- `BackgroundUpdate` should only replace `MiniDspReady`.
- `BackgroundUpdate` should not override blue or amber states.
- If the box is not currently usable for MiniDSP control, show the primary state, not update activity.

## Brightness Recommendation

Default to low brightness.

Reasonable starting range:

- idle/status states: `8-24` per color channel out of `255`
- short attention states: `24-48`
- avoid `255,255,255` except for a brief boot self-test if needed

Full white on these boards is unnecessarily bright for a rack/device environment.

## Color Order Recommendation

Use `GRB` for the currently tested board.

Why:

- community examples for this board family often use `GRB`
- some boards are known to have swapped color order
- the current board under test displayed the full color verification loop correctly with `GRB`

Verification rule:

- if "red" shows green, fix the color order in software first
- do not immediately assume the pin is wrong

## Integration Points In The Current Firmware

The existing firmware now drives LED updates from these coarse transition points:

Good hook points:

- `app-esp32/src/main.rs`
  - boot startup
  - initial STA vs SoftAP outcome
  - USB absent vs protocol-ready
  - fatal startup failures
- `app-esp32/src/wifi.rs`
  - reconnect loop active
  - STA reconnect success
  - provisioning fallback after repeated credential failure
- `app-esp32/src/updater.rs`
  - real catalogue update/download work

The LED should mirror coarse state transitions, not every low-level event.

## Production Integration Status

The real firmware now uses the following behavior:

- keep the existing direct `esp-idf-hal` RMT implementation in `app-esp32/src/led.rs`
- keep `GPIO48`, one pixel, `GRB`
- treat `led-demo` as a bring-up/tuning tool only, not as the production control path
- preserve the current state meanings and color semantics:
  - `Booting`: solid amber
  - `ProvisioningAp`: blue breathe to black
  - `WifiConnecting`: blue flash
  - `MiniDspUnavailable`: amber breathe between `AMBER_MIN` and `AMBER`
  - `MiniDspReady`: dim solid green
  - `BackgroundUpdate`: green breathe to black
  - `Fatal`: solid red
- preserve the current breathing style:
  - period `4.5 s`
  - endpoint dwell about `180 ms`
  - amber does not go to black
  - blue and green may go to black
- keep the hue-preserving low-end scaling logic; do not revert to naive per-channel scaling

Production-integration notes:

- the current `DEMO_HOLD` and `TRANSITION_GAP` values are for demo loops only and should not drive the real state machine
- the real firmware now updates LED state on actual system transitions, not on fixed timers
- one controller thread owns the LED handle and consumes coarse state updates from the rest of the firmware
- `Fatal` is latched in the controller so later WiFi-mode updates do not clear a hard failure indication
- if the production path needs smoother or cheaper updates, optimize around the existing `led.rs` code rather than replacing it with a simpler GPIO blink path

## Current Production Plumbing

The current production implementation does not require an extra ESP-IDF `led_strip` component. It uses `esp-idf-hal`'s RMT support directly for the one-pixel onboard LED.

If a future implementation needs more complex strip support or wants to standardize on Espressif's `led_strip` component, re-evaluate that decision then.

## Demo Binary

There is now a dedicated demo binary for bring-up and visual review:

- source: `app-esp32/src/bin/led-demo.rs`
- production driver: `app-esp32/src/led.rs`
- demo helper module: `app-esp32/src/led_demo.rs`

Build it with:

```bash
cd app-esp32
esp cargo build --bin led-demo
```

The current `led-demo` binary loops forever through the amber-only breathe tuning pass:

1. low amber
2. ramp to full amber
3. tiny hold at full
4. ramp back to low amber
5. repeat

Current amber demo timing:

- breathing period: `4.5 s`
- breathing endpoint dwell: about `180 ms`
- low endpoint: low amber, not black

Other demo-helper paths still exist in `app-esp32/src/led_demo.rs`:

- raw color verification loop for pin/order bring-up
- full product-state demo for reviewing all status colors/timing

## Verification Checklist

Before relying on the LED in production firmware:

1. `GPIO48` is verified on the tested board with the color loop.
2. `GRB` ordering is verified on the tested board with the color loop.
3. The current state-demo timing and brightness have been visually tuned on the tested board.
4. The production controller is wired to real firmware transitions in `main.rs`, `wifi.rs`, and `updater.rs`.
5. Blue and amber production states were visually spot-checked on the tested board after flashing the integrated firmware on 2026-04-12.
6. Verify the LED remains stable while WiFi is enabled.
7. Verify the chosen pattern set is readable from across the room.
8. Verify the "healthy idle" state is not visually obnoxious in a dark room.
9. Verify boot, provisioning, device attach, device loss, and update paths all produce the intended state.
10. If any board fails the test, capture the exact board revision and add it to this document before adding board-specific code paths.

## Bottom Line

For the boards currently used by `minidsp-open`, the onboard RGB LED should be treated as:

- present
- connected
- one addressable `WS2812`-class pixel
- driven from `GPIO48`
- known-good with `GRB` ordering on the tested board
- currently driven successfully by the repo's direct RMT implementation in both demo and production firmware
- current state-demo tuning on the tested board is `10 s` per state with `4.5 s` breathing and a tiny endpoint dwell
- production LED state control now lives in the real firmware rather than only in the demo binary
- still a candidate for future `led_strip` integration if a higher-level driver becomes useful

That is the right assumption until a specific board revision proves otherwise.

## Sources

External sources used to build this note:

- VCC-GND board repo: https://github.com/vcc-gnd/YD-ESP32-S3
- VCC-GND RGB jumper issue: https://github.com/vcc-gnd/YD-ESP32-S3/issues/1
- VCC-GND RGB example issue: https://github.com/vcc-gnd/YD-ESP32-S3/issues/4
- Board-family manual page: https://manuals.plus/ae/1005008700617854
- Product listing family with onboard WS2812 LED: https://www.aliexpress.com/item/1005006370275059.html
- Espressif LED strip component: https://components.espressif.com/components/espressif/led_strip/versions/2.5.4/readme?language=en
- Espressif RMT API docs: https://docs.espressif.com/projects/esp-idf/en/stable/esp32s3/api-reference/peripherals/rmt.html
- Espressif RMT FAQ: https://docs.espressif.com/projects/esp-faq/en/latest/software-framework/peripherals/rmt.html

Local sources:

- `docs/ref-esp32-hardware.md`
- `app-esp32/Cargo.toml`
- `app-esp32/src/main.rs`
