//! Shared raw-bridge command execution for WebSocket and TCP daemon transports.

use crate::state::DaemonState;
use minidsp::client::MiniDspClient;
use minidsp::queue::Priority;
use minidsp_protocol::commands;
use minidsp_protocol::eeprom;
use std::sync::Arc;

/// Execute a raw bridge command against the live device and return the raw
/// MiniDSP payload that should be framed back to the client.
///
/// `SetConfig`, `Reset`, and `EnterBootloader` are special on the physical
/// 2x4 HD: the command can succeed while never yielding a usable raw
/// completion frame. Route them through the verified high-level client path
/// and synthesize the standard completion payload so daemon WS/TCP clients
/// still see a successful response.
pub async fn execute_raw_bridge_command(
    state: &DaemonState,
    client: &Arc<MiniDspClient>,
    payload: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let request = payload.clone();

    if payload.first().copied() == Some(commands::CMD_SET_CONFIG) && payload.len() >= 2 {
        let preset = payload[1];
        let reset_mode = payload
            .get(2)
            .copied()
            .unwrap_or(commands::PRESET_CHANGE_NORMAL);
        client
            .set_config_with_mode(preset, reset_mode)
            .await
            .map_err(|e| e.to_string())?;

        let raw_resp = vec![if reset_mode == commands::PRESET_CHANGE_WITHOUT_RESET {
            commands::RESP_OK
        } else {
            commands::RESP_PRESET_COMPLETE
        }];
        client
            .mirror_raw_command_if_success(&request, &raw_resp)
            .await;
        state.persist_device_shadow(client).await;
        return Ok(raw_resp);
    }

    if payload.first().copied() == Some(commands::CMD_RESET) {
        client
            .reset_with_verification()
            .await
            .map_err(|e| e.to_string())?;

        let raw_resp = vec![commands::RESP_RESET_COMPLETE];
        client
            .mirror_raw_command_if_success(&request, &raw_resp)
            .await;
        state.persist_device_shadow(client).await;
        return Ok(raw_resp);
    }

    if payload.first().copied() == Some(commands::CMD_ENTER_BOOTLOADER) {
        client.reboot_device().await.map_err(|e| e.to_string())?;

        let raw_resp = vec![commands::RESP_RESET_COMPLETE];
        client
            .mirror_raw_command_if_success(&request, &raw_resp)
            .await;
        state.persist_device_shadow(client).await;
        return Ok(raw_resp);
    }

    let raw_resp = client
        .queue()
        .execute_raw(payload, Priority::Normal)
        .await
        .map_err(|e| e.to_string())?;
    client
        .mirror_raw_command_if_success(&request, &raw_resp)
        .await;

    // Modification-token bookkeeping for raw bridge writes:
    //
    // Device Console writes a fresh token after every UI mutation as part of
    // its `updateModificationToken()` flow. When DC drives our daemon over
    // the TCP bridge, IT sends both the parameter write AND a follow-up
    // WriteFlash to `0xFFC8 + slot*4` — so we don't need to bump on DC's
    // behalf, just refresh our local token shadow afterwards.
    //
    // But if a non-DC bridge client (a custom tool, an old minidsp-rs
    // session, ezBEQ via TCP, etc.) sends a parameter write WITHOUT a
    // follow-up token write, the device's flash token will stay stale and
    // any cached Device Console session will not detect the drift. To match
    // the official cache-coherency model in the cross-client case, classify
    // the request and bump the token after a successful preset content
    // mutation that was NOT itself a token write.
    if is_token_bumping_write(&request) {
        state
            .persist_device_shadow_after_preset_mutation(client)
            .await;
    } else {
        state.persist_device_shadow(client).await;
    }
    Ok(raw_resp)
}

/// Returns true if `payload` is a parameter / preset-content mutation whose
/// success should bump the active preset's modification token.
///
/// Excludes:
/// - reads (`CMD_READ_*`),
/// - master controls (`CMD_SET_VOLUME`, `CMD_SET_MUTE`, `CMD_SET_SOURCE`),
/// - preset switches and resets (handled by their dedicated paths above),
/// - writes targeting the modification-token flash region itself — those are
///   already-bump-correct token-set requests from a smart client (e.g. DC),
///   re-bumping would invalidate the token they just wrote.
fn is_token_bumping_write(payload: &[u8]) -> bool {
    let Some(opcode) = payload.first().copied() else {
        return false;
    };
    match opcode {
        commands::CMD_WRITE_DSP
        | commands::CMD_WRITE_BIQUAD
        | commands::CMD_BYPASS_FILTER
        | commands::CMD_FIR_LOAD_DATA
        | commands::CMD_RELOAD_DSP_PARAM
        | commands::CMD_WRITE_FLASH_BLOCK
        | commands::CMD_FINISH_FLASH_BLOCK
        | commands::CMD_COPY_PRESET => true,
        commands::CMD_WRITE_FLASH => !is_token_flash_write(payload),
        commands::CMD_WRITE_FLASH_FULL_ADDR => !is_token_flash_full_addr_write(payload),
        _ => false,
    }
}

/// Detect a `CMD_WRITE_FLASH` command targeting the modification-token region
/// at `0xFFC8..0xFFD8` (4 preset slots × 4 bytes). Layout:
/// `[0x04][hi][lo][..data]`. We deliberately match on the address bytes only.
fn is_token_flash_write(payload: &[u8]) -> bool {
    if payload.len() < 3 {
        return false;
    }
    let hi = payload[1];
    let lo = payload[2];
    if hi != eeprom::TOKENS_ADDR.0 {
        return false;
    }
    let token_lo_start = eeprom::TOKENS_ADDR.1;
    let token_lo_end = token_lo_start + 4 * eeprom::TOKEN_SIZE_BYTES;
    (token_lo_start..token_lo_end).contains(&lo)
}

/// Same as [`is_token_flash_write`] but for the extended-address opcode
/// `CMD_WRITE_FLASH_FULL_ADDR`. Layout: `[0x3C][addr_be32][..data]`.
fn is_token_flash_full_addr_write(payload: &[u8]) -> bool {
    if payload.len() < 5 {
        return false;
    }
    let addr = u32::from_be_bytes([payload[1], payload[2], payload[3], payload[4]]);
    let token_full = u32::from_be_bytes([eeprom::TOKENS_ADDR.0, eeprom::TOKENS_ADDR.1, 0, 0]) >> 16;
    let token_lo_start: u32 = ((eeprom::TOKENS_ADDR.0 as u32) << 8) | eeprom::TOKENS_ADDR.1 as u32;
    let token_lo_end = token_lo_start + 4 * eeprom::TOKEN_SIZE_BYTES as u32;
    let _ = token_full;
    (token_lo_start..token_lo_end).contains(&addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_commands_do_not_bump_token() {
        // CMD_READ_FLOATS / CMD_READ_FLASH / CMD_READ_HARDWARE_ID / etc.
        // None of these mutate any preset state.
        for cmd in [0x05u8, 0x14, 0x31, 0x40] {
            assert!(
                !is_token_bumping_write(&[cmd, 0xff, 0xd8, 0x04]),
                "read opcode 0x{cmd:02x} should not bump"
            );
        }
    }

    #[test]
    fn master_status_writes_do_not_bump_token() {
        // SetVolume / SetMute / SetSource are device-wide controls that
        // don't change preset content.
        assert!(!is_token_bumping_write(&[commands::CMD_SET_VOLUME, 0x00]));
        assert!(!is_token_bumping_write(&[commands::CMD_SET_MUTE, 0x01]));
        assert!(!is_token_bumping_write(&[commands::CMD_SET_SOURCE, 0x00]));
    }

    #[test]
    fn parameter_writes_bump_token() {
        // WriteDSP / WriteBiquad / BypassFilter / etc. all need a bump.
        assert!(is_token_bumping_write(&[
            commands::CMD_WRITE_DSP,
            0x80,
            0x00,
            0x2a,
            0x00,
            0x00,
            0x80,
            0x40
        ]));
        assert!(is_token_bumping_write(&[commands::CMD_WRITE_BIQUAD, 0x80]));
        assert!(is_token_bumping_write(&[commands::CMD_BYPASS_FILTER, 0x80]));
        assert!(is_token_bumping_write(&[commands::CMD_FIR_LOAD_DATA, 0x00]));
        assert!(is_token_bumping_write(&[commands::CMD_RELOAD_DSP_PARAM]));
    }

    #[test]
    fn flash_writes_outside_token_region_bump() {
        // Writes to non-token flash regions (e.g. 0xFFEA selectable source
        // descriptor) are treated as bump-eligible mutations since we don't
        // know the semantics from opcode alone.
        let payload = [commands::CMD_WRITE_FLASH, 0xFF, 0xEA, 0x01, 0x02];
        assert!(is_token_bumping_write(&payload));
    }

    #[test]
    fn flash_writes_targeting_token_region_do_not_bump() {
        // Device Console's `saveModificationTokenToDevice` writes 4 bytes to
        // 0xFFC8 + slot*4. Our daemon must NOT re-bump those writes —
        // otherwise we'd overwrite DC's freshly chosen token with our own
        // random one and DC would see drift on its very next read.
        for slot in 0u8..4 {
            let lo = eeprom::TOKENS_ADDR.1 + slot * eeprom::TOKEN_SIZE_BYTES;
            let payload = [commands::CMD_WRITE_FLASH, 0xFF, lo, 0x11, 0x22, 0x33, 0x44];
            assert!(
                !is_token_bumping_write(&payload),
                "WriteFlash 0xFF{lo:02X} (token slot {slot}) should NOT bump"
            );
        }

        // Just outside the token region (0xFFD8 = master status, after the
        // last token slot 0xFFD4) — that's NOT a token write.
        let payload = [commands::CMD_WRITE_FLASH, 0xFF, 0xD8, 0x00];
        assert!(
            is_token_bumping_write(&payload),
            "WriteFlash 0xFFD8 (master status) is not a token region write"
        );
    }

    #[test]
    fn copy_preset_command_bumps() {
        assert!(is_token_bumping_write(&[commands::CMD_COPY_PRESET, 0x01]));
    }
}
