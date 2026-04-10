//! Shared raw-bridge command execution for WebSocket and TCP daemon transports.

use crate::state::DaemonState;
use minidsp::client::MiniDspClient;
use minidsp::queue::Priority;
use minidsp_protocol::commands;
use std::sync::Arc;

/// Execute a raw bridge command against the live device and return the raw
/// MiniDSP payload that should be framed back to the client.
///
/// `SetConfig` and `Reset` are special on the physical 2x4 HD: the command can
/// succeed while never yielding a usable raw completion frame. Route them
/// through the verified high-level client path and synthesize the standard
/// completion payload so daemon WS/TCP clients still see a successful response.
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

        let raw_resp = vec![commands::RESP_PRESET_COMPLETE];
        client.mirror_raw_command_if_success(&request, &raw_resp).await;
        state.persist_device_shadow(client).await;
        return Ok(raw_resp);
    }

    if payload.first().copied() == Some(commands::CMD_RESET) {
        client
            .reset_with_verification()
            .await
            .map_err(|e| e.to_string())?;

        let raw_resp = vec![commands::RESP_RESET_COMPLETE];
        client.mirror_raw_command_if_success(&request, &raw_resp).await;
        state.persist_device_shadow(client).await;
        return Ok(raw_resp);
    }

    let raw_resp = client
        .queue()
        .execute_raw(payload, Priority::Normal)
        .await
        .map_err(|e| e.to_string())?;
    client.mirror_raw_command_if_success(&request, &raw_resp).await;
    state.persist_device_shadow(client).await;
    Ok(raw_resp)
}
