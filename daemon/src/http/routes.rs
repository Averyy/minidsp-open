//! Axum route definitions and handlers.

use crate::http::json::*;
use crate::state::DaemonState;
use axum::{
    Json,
    extract::{FromRequest, Path, State, Query, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tower_http::cors::{CorsLayer, Any};

pub fn router(state: Arc<DaemonState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/devices", get(list_devices))
        .route("/devices/{id}", get(device_get_or_ws).post(set_device_status))
        .route("/devices/{id}/config", post(write_config).get(read_config))
        .route("/devices/{id}/volume/{direction}", post(volume_direction))
        .route("/devices/{id}/source/{source}", post(set_source))
        .route("/devices/{id}/preset/{preset}", post(set_preset))
        .route("/devices/{id}/display", get(get_display).post(set_display))
        .route("/devices/{id}/dre", get(get_dre).post(set_dre))
        .route("/devices/{id}/measurement-mode", post(enter_measurement_mode))
        .route("/devices/{id}/measurement-mode/restore", post(restore_measurement_mode))
        .route("/devices/{id}/tokens", get(get_tokens))
        .route("/devices/{id}/preset/copy", post(copy_preset))
        .route("/devices/{id}/ws", get(ws_raw_bridge))
        .route("/openapi.json", get(openapi))
        .route("/llms.txt", get(llms_txt))
        .layer(cors)
        .with_state(state)
}

// -- WebSocket query params --

#[derive(Debug, Deserialize, Default)]
struct WsQueryParams {
    #[serde(default)]
    poll: Option<bool>,
    #[serde(default)]
    levels: Option<bool>,
}

/// GET /devices/{id} — returns JSON status OR upgrades to WebSocket for state/levels.
/// This is the compatibility endpoint: ezBEQ connects with `?levels=true&poll=true`.
async fn device_get_or_ws(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
    Query(params): Query<WsQueryParams>,
    req: axum::extract::Request,
) -> Result<axum::response::Response, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;

    // Check if this is a WebSocket upgrade request
    let is_ws = req.headers().get("upgrade")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));

    if is_ws {
        let ws = WebSocketUpgrade::from_request(req, &state)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let poll = params.poll.unwrap_or(false);
        let levels = params.levels.unwrap_or(false);
        return Ok(ws.on_upgrade(move |socket| ws_state_levels(socket, client, poll, levels)).into_response());
    }

    // Otherwise return JSON status
    let status = client
        .get_status_summary()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(StatusSummaryJson {
        master: MasterStatusGetJson {
            preset: status.master.preset,
            source: status.master.source,
            volume: status.master.volume,
            mute: status.master.mute,
            dirac: status.master.dirac,
        },
        input_levels: status.input_levels,
        output_levels: status.output_levels,
    }).into_response())
}

/// WebSocket: state + level streaming (compatibility endpoint on /devices/{id}).
async fn ws_state_levels(
    mut socket: WebSocket,
    client: Arc<minidsp::client::MiniDspClient>,
    poll: bool,
    levels: bool,
) {
    let interval_ms = if levels { 250 } else if poll { 1000 } else { 5000 };
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(interval_ms));

    loop {
        tokio::select! {
            // Send status on each tick
            _ = tick.tick() => {
                match client.get_status_summary().await {
                    Ok(status) => {
                        let json = serde_json::json!({
                            "master": {
                                "preset": status.master.preset,
                                "source": status.master.source,
                                "volume": status.master.volume,
                                "mute": status.master.mute,
                                "dirac": status.master.dirac,
                            },
                            "input_levels": status.input_levels,
                            "output_levels": status.output_levels,
                        });
                        if socket.send(Message::Text(json.to_string().into())).await.is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            // Listen for client close/messages
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Err(_)) => return,
                    _ => {} // ignore pings, text from client
                }
            }
        }
    }
}

/// WebSocket: raw frame bridge (on /devices/{id}/ws).
async fn ws_raw_bridge(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    Ok(ws.on_upgrade(move |socket| ws_raw_handler(socket, client)))
}

async fn ws_raw_handler(
    mut socket: WebSocket,
    client: Arc<minidsp::client::MiniDspClient>,
) {
    use minidsp::queue::Priority;

    while let Some(msg) = socket.recv().await {
        match msg {
            Ok(Message::Binary(data)) => {
                // Decode frame and execute
                if let Some(payload) = minidsp_protocol::packet::decode_frame(&data) {
                    match client.queue().execute_raw(payload, Priority::Normal).await {
                        Ok(resp) => {
                            let frame = minidsp_protocol::packet::encode_frame(&resp);
                            if socket.send(Message::Binary(frame.into())).await.is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => return,
            _ => {}
        }
    }
}

// -- Helpers --

/// Read Dirac bypass state from EEPROM, or None if device doesn't support Dirac.
async fn read_dirac_state(client: &minidsp::client::MiniDspClient) -> Option<bool> {
    if !client.profile().has_dirac {
        return None;
    }
    match client.raw_command(
        minidsp_protocol::commands::read_flash(0xFF, 0xE0, 1),
        minidsp::queue::Priority::Normal,
    ).await {
        Ok(minidsp_protocol::commands::Response::FlashData(data)) if !data.is_empty() => {
            Some(data[0] == 0) // 0 = active, 1 = bypassed
        }
        _ => Some(false),
    }
}

// -- Standard REST handlers --

async fn list_devices(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let devices = state.devices.read().await;
    let mut list = Vec::new();
    for (&id, client) in devices.iter() {
        let url = format!("ws://localhost:5380/devices/{id}/ws");
        let info = client.device_info(&url);
        list.push(DeviceJson {
            url: info.url,
            version: VersionJson {
                hw_id: info.version.hw_id,
                fw_major: info.version.fw_major,
                fw_minor: info.version.fw_minor,
                dsp_version: info.version.dsp_version,
                serial: info.version.serial,
            },
            product_name: info.product_name,
        });
    }
    Json(list)
}

async fn set_device_status(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
    Json(body): Json<MasterStatusPostJson>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;

    if let Some(volume) = body.volume {
        client.set_volume_db(volume).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if let Some(mute) = body.mute {
        client.set_mute(mute).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if let Some(preset) = body.preset {
        client.set_config(preset).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if let Some(source) = &body.source {
        if let Some(src) = minidsp_protocol::source::Source::from_str_pascal(source) {
            if let Some(idx) = client.profile().sources.index_for_source(src) {
                client.set_source(idx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
        }
    }
    if let Some(dirac) = body.dirac {
        let _ = client.set_dirac_bypass(!dirac).await;
    }

    let master = client.get_master_status().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_name = client.profile().sources.source_for_index(master.source).map(|s| s.pascal_case().to_string());
    let dirac = read_dirac_state(&client).await;
    Ok(Json(MasterStatusResponseJson {
        preset: Some(master.preset),
        source: source_name,
        volume: Some(master.volume_db()),
        mute: Some(master.is_muted()),
        dirac,
    }))
}

async fn volume_direction(
    State(state): State<Arc<DaemonState>>,
    Path((id, direction)): Path<(usize, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let master = client.get_master_status().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match direction.as_str() {
        "up" => {
            let new_vol = master.volume.saturating_sub(1);
            client.set_volume(new_vol).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        "down" => {
            let new_vol = master.volume.saturating_add(1);
            client.set_volume(new_vol).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        "toggle_mute" => {
            client.set_mute(!master.is_muted()).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    }

    let updated = client.get_master_status().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_name = client.profile().sources.source_for_index(updated.source).map(|s| s.pascal_case().to_string());
    let dirac = read_dirac_state(&client).await;
    Ok(Json(MasterStatusResponseJson {
        preset: Some(updated.preset),
        source: source_name,
        volume: Some(updated.volume_db()),
        mute: Some(updated.is_muted()),
        dirac,
    }))
}

async fn set_source(
    State(state): State<Arc<DaemonState>>,
    Path((id, source)): Path<(usize, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let src = minidsp_protocol::source::Source::from_str_lowercase(&source)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let idx = client.profile().sources.index_for_source(src)
        .ok_or(StatusCode::BAD_REQUEST)?;
    client.set_source(idx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let master = client.get_master_status().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_name = client.profile().sources.source_for_index(master.source).map(|s| s.pascal_case().to_string());
    let dirac = read_dirac_state(&client).await;
    Ok(Json(MasterStatusResponseJson {
        preset: Some(master.preset),
        source: source_name,
        volume: Some(master.volume_db()),
        mute: Some(master.is_muted()),
        dirac,
    }))
}

async fn set_preset(
    State(state): State<Arc<DaemonState>>,
    Path((id, preset)): Path<(usize, u8)>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    client.set_config(preset).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let master = client.get_master_status().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_name = client.profile().sources.source_for_index(master.source).map(|s| s.pascal_case().to_string());
    let dirac = read_dirac_state(&client).await;
    Ok(Json(MasterStatusResponseJson {
        preset: Some(master.preset),
        source: source_name,
        volume: Some(master.volume_db()),
        mute: Some(master.is_muted()),
        dirac,
    }))
}

async fn write_config(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
    Json(body): Json<ConfigWriteJson>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;

    if let Some(ms) = body.master_status {
        if let Some(preset) = ms.preset {
            client.set_config(preset).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        if let Some(volume) = ms.volume {
            client.set_volume_db(volume).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        if let Some(mute) = ms.mute {
            client.set_mute(mute).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        if let Some(source) = &ms.source {
            if let Some(src) = minidsp_protocol::source::Source::from_str_pascal(source) {
                if let Some(idx) = client.profile().sources.index_for_source(src) {
                    client.set_source(idx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                }
            }
        }
    }

    if let Some(inputs) = body.inputs {
        let table = client.profile().dsp_table.ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        for input in inputs {
            if let Some(input_ch) = table.inputs.get(input.index as usize) {
                if let Some(peqs) = input.peq {
                    for peq in peqs {
                        if let Some(band) = input_ch.peq.get(peq.index as usize) {
                            if let Some(coeff) = peq.coeff {
                                let coeffs = [coeff.b0, coeff.b1, coeff.b2, coeff.a1, coeff.a2];
                                client.write_biquad(band.addr, &coeffs)
                                    .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                            }
                            if let Some(bypass) = peq.bypass {
                                client.bypass_filter(band.addr, bypass)
                                    .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(outputs) = body.outputs {
        let table = client.profile().dsp_table.ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        for output in outputs {
            if let Some(output_ch) = table.outputs.get(output.index as usize) {
                if let Some(peqs) = output.peq {
                    for peq in peqs {
                        if let Some(band) = output_ch.peq.get(peq.index as usize) {
                            if let Some(coeff) = peq.coeff {
                                let coeffs = [coeff.b0, coeff.b1, coeff.b2, coeff.a1, coeff.a2];
                                client.write_biquad(band.addr, &coeffs)
                                    .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                            }
                            if let Some(bypass) = peq.bypass {
                                client.bypass_filter(band.addr, bypass)
                                    .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(StatusCode::OK)
}

async fn read_config(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let table = client.profile().dsp_table.ok_or(StatusCode::NOT_IMPLEMENTED)?;

    // Read all output PEQ values
    let mut outputs = Vec::new();
    for (ch_idx, output) in table.outputs.iter().enumerate() {
        let mut peqs = Vec::new();
        for (band_idx, band) in output.peq.iter().enumerate() {
            match client.read_floats(band.addr, 5).await {
                Ok(coeffs) if coeffs.len() == 5 => {
                    peqs.push(PeqConfigJson {
                        index: band_idx as u8,
                        coeff: Some(BiquadCoeffJson {
                            b0: coeffs[0], b1: coeffs[1], b2: coeffs[2],
                            a1: coeffs[3], a2: coeffs[4],
                        }),
                        bypass: None, // TODO: read bypass state
                    });
                }
                _ => {}
            }
        }
        outputs.push(InputConfigJson {
            index: ch_idx as u8,
            peq: Some(peqs),
            gain: None,
            mute: None,
        });
    }

    Ok(Json(serde_json::json!({
        "outputs": outputs,
    })).into_response())
}

async fn get_display(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    if !client.profile().has_display {
        return Err(StatusCode::NOT_FOUND);
    }
    // Read display settings from EEPROM
    let resp = client.raw_command(
        minidsp_protocol::commands::read_flash(0xFF, 0xE8, 2),
        minidsp::queue::Priority::Normal,
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match resp {
        minidsp_protocol::commands::Response::FlashData(data) if data.len() >= 2 => {
            Ok(Json(serde_json::json!({
                "idle_time": data[0],
                "brightness": data[1],
            })).into_response())
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn set_display(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    if !client.profile().has_display {
        return Err(StatusCode::NOT_FOUND);
    }
    if let Some(brightness) = body.get("brightness").and_then(|v| v.as_u64()) {
        client.raw_command(
            minidsp_protocol::commands::set_display_brightness(brightness as u8),
            minidsp::queue::Priority::High,
        ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if let Some(idle_time) = body.get("idle_time").and_then(|v| v.as_u64()) {
        client.raw_command(
            minidsp_protocol::commands::set_display_idle_time(idle_time as u8),
            minidsp::queue::Priority::High,
        ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(StatusCode::OK)
}

async fn get_dre(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    if !client.profile().has_dre {
        return Err(StatusCode::NOT_FOUND);
    }
    let resp = client.raw_command(
        minidsp_protocol::commands::read_flash(0xFF, 0xED, 1),
        minidsp::queue::Priority::Normal,
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match resp {
        minidsp_protocol::commands::Response::FlashData(data) if !data.is_empty() => {
            Ok(Json(serde_json::json!({ "enabled": data[0] != 0 })).into_response())
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn set_dre(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    if !client.profile().has_dre {
        return Err(StatusCode::NOT_FOUND);
    }
    if let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) {
        client.raw_command(
            minidsp_protocol::commands::set_dre(enabled),
            minidsp::queue::Priority::High,
        ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(StatusCode::OK)
}

async fn get_tokens(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let resp = client.raw_command(
        minidsp_protocol::commands::read_flash(0xFF, 0xC8, 16),
        minidsp::queue::Priority::Normal,
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match resp {
        minidsp_protocol::commands::Response::FlashData(data) => {
            let mut tokens = Vec::new();
            for chunk in data.chunks(4) {
                if chunk.len() == 4 {
                    tokens.push(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            Ok(Json(serde_json::json!({ "tokens": tokens })).into_response())
        }
        _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn copy_preset(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    client.raw_command(
        minidsp_protocol::commands::copy_preset(true),
        minidsp::queue::Priority::Critical,
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn enter_measurement_mode(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let _client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    // TODO: save current PEQ state, flatten all PEQ to bypass
    Err(StatusCode::NOT_IMPLEMENTED)
}

async fn restore_measurement_mode(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let _client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    // TODO: restore saved PEQ state
    Err(StatusCode::NOT_IMPLEMENTED)
}

async fn openapi() -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

async fn llms_txt() -> impl IntoResponse {
    (StatusCode::OK, "# minidsp-open\n\nMiniDSP controller HTTP API.\nSee /openapi.json for endpoints.\n")
}
