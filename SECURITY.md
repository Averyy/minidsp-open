# Security

minidsp-open trusts clients on its LAN. Keep its HTTP API and bridges off the public Internet and untrusted networks.

Browser mutations and WebSocket upgrades require a local Origin (`localhost`, single-label, `.local` or home-suffix names, private IPs, or an IP literal addressing itself) or an explicitly configured one. Pages on public DNS names are refused even when their Host matches, which also blocks DNS rebinding. Native clients without Origin keep working. CORS echoes allowed origins only. The daemon's explicit `cors_origin = "*"` disables this restriction. WiFi credentials can only be changed in provisioning mode. Firmware uploads and catalogue source overrides are unavailable in provisioning; source overrides also require a development build feature.

Origin checks do not authenticate LAN clients. These limits are deliberate or remain open:

- OTA firmware is unsigned. Anyone on the LAN can flash firmware, read stored secrets through replacement firmware, and control the connected device.
- The provisioning AP is open and sends credentials over HTTP; protection is tracked as P3 in [PROGRESS.md](PROGRESS.md).
- The raw TCP bridge is unauthenticated by design for minidsp-rs compatibility.
- The desktop daemon deliberately binds HTTP to `0.0.0.0` and uses a `0766` Unix socket.
- Set the optional maximum master volume through `/settings` (or ESP32 `/status`) to bound master-volume writes from HTTP, WebSocket, TCP bridge, and rattle playback. It is off by default. LAN clients can also raise or clear this setting; it is a safety control, not authentication.
- The MiniDSP front-panel knob and IR remote bypass minidsp-open, so the ceiling cannot stop them. Direct USB CLI use also bypasses the daemon ceiling; CLI requests through the daemon URL/socket or a minidsp-open TCP bridge are bounded.

Report vulnerabilities through [GitHub private vulnerability reporting](https://github.com/Averyy/minidsp-open/security/advisories/new). Include the affected version, reproduction steps, and impact. Please avoid public issues for undisclosed vulnerabilities.
