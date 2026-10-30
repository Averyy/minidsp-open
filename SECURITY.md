# Security

minidsp-open trusts clients on its LAN. Keep its HTTP API and bridges off the public Internet and untrusted networks.

Browser mutations and WebSocket upgrades require a local Origin (`localhost`, single-label, `.local` or home-suffix names, private IPs, or an IP literal addressing itself) or an explicitly configured one. Pages on public DNS names are refused even when their Host matches, which also blocks DNS rebinding. Native clients without Origin keep working. CORS echoes allowed origins only. The daemon's explicit `cors_origin = "*"` disables this restriction. WiFi credentials can only be changed in provisioning mode. Firmware uploads and catalogue source overrides are unavailable in provisioning; source overrides also require a development build feature.

Origin checks do not authenticate LAN clients. The ESP32 offers an **optional password**, off by default: open LAN control remains the default. Set, change or clear it under `/status` → Device settings → Password, or optionally set it during first WiFi provisioning. Changing/clearing requires a login and the current password. It covers all UI assets, API reads/writes, settings, diagnostics, playback/rattle, OTA uploads and WebSocket upgrades. Only the self-contained login page, captive probes/DNS and provisioning landing page are public; saving WiFi still requires authentication in a protected fallback AP.

Browsers use an HttpOnly, SameSite=Strict cookie; native clients can use Basic (`admin`) or a Bearer session token returned by `POST /login`. Eight RAM sessions expire after 30 minutes idle or 12 hours absolute, and are lost on reboot. Logout revokes its session; changing/clearing the password revokes all sessions and closes WebSockets. Successful Basic credentials are cached as a per-boot HMAC for five minutes. A Basic WebSocket stays open until the password changes. Bounded per-IP and global failure budgets apply capped backoff (at most 64 s) before password derivation, without sleeping. A single address renewing its own backoff can't lock out other clients, and logged-in password changes skip the global budget; several addresses guessing at once can still delay every login. Passwords are stored as versioned, randomly salted PBKDF2-HMAC-SHA256 verifiers (10,000 iterations initially; hardware timing remains to be measured).

While protected, the firmware closes existing TCP 5333 connections, stops listening for new ones and stops UDP 3999 advertisements. Clearing the password resumes both. mDNS remains available. Device Console and the mobile app cannot authenticate to this bridge. The desktop daemon has no new authentication gate.

**Plain HTTP does not encrypt passwords or tokens.** Anyone able to intercept LAN traffic can capture and reuse them. HttpOnly/SameSite and Origin checks do not prevent network interception. Keep the device off the Internet and use a trusted network.

For recovery, after the board has booted normally, hold BOOT (GPIO0) for ten seconds; the RGB LED flashes red/blue while held. This clears the password and saved WiFi, revokes sessions and restarts into provisioning. DSP settings, the volume ceiling and catalogue data remain. If the firmware can't read the stored password (flash corruption, or a newer record format after a rollback), it stays locked rather than opening up, and this gesture clears it. Do not hold BOOT during reset: that enters the ROM downloader. There is no serial command console. If the gesture is unavailable, use `tools/esptool --chip esp32s3 -p PORT erase-region 0x9000 0x6000` over USB, then restart; this erases all NVS settings, including the ceiling. Reflashing an app image alone preserves NVS and does not remove the password.

These limits are deliberate or remain open:

- OTA firmware is unsigned. With the password off, anyone on the LAN can upload replacement firmware and control the device. With protection on, uploads require authentication; an authenticated uploader or a person with physical flash access can still replace firmware and read stored secrets.
- The provisioning AP is open and uses unencrypted HTTP. A configured device password still gates provisioning mutations.
- When the optional password is off, the raw TCP bridge is unauthenticated for minidsp-rs compatibility.
- The desktop daemon deliberately binds HTTP to `0.0.0.0` and uses a `0766` Unix socket.
- Set the optional maximum master volume through `/settings` (or ESP32 `/status`) to bound master-volume writes from HTTP, WebSocket, TCP bridge, and rattle playback. It is off by default. Authorized clients (any LAN client when no password is set) can also raise or clear this setting; it is a safety control, not authentication.
- The MiniDSP front-panel knob and IR remote bypass minidsp-open, so the ceiling cannot stop them. Direct USB CLI use also bypasses the daemon ceiling; CLI requests through the daemon URL/socket or a minidsp-open TCP bridge are bounded.

Report vulnerabilities through [GitHub private vulnerability reporting](https://github.com/Averyy/minidsp-open/security/advisories/new). Include the affected version, reproduction steps, and impact. Please avoid public issues for undisclosed vulnerabilities.
