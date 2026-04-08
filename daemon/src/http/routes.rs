//! Axum route definitions and handlers.

use crate::http::json::*;
use crate::state::DaemonState;
use axum::{
    Json, Router,
    extract::{
        FromRequest, Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use minidsp_protocol::playback::{SyncRequest, SyncResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tower_http::cors::{Any, CorsLayer};

/// JSON error response — minidsp-rs compatible format: `{"error": "message"}`.
#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
}

impl ApiError {
    fn new(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Self>) {
        (status, Json(Self { error: msg.into() }))
    }
    fn not_found() -> (StatusCode, Json<Self>) {
        Self::new(StatusCode::NOT_FOUND, "device not found")
    }
    fn internal(msg: impl Into<String>) -> (StatusCode, Json<Self>) {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, msg)
    }
    fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<Self>) {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
}

/// Result type alias for handlers returning JSON errors.
type ApiResult<T> = Result<T, (StatusCode, Json<ApiError>)>;

/// Get device or return JSON 404.
async fn get_device_or_404(
    state: &DaemonState,
    id: usize,
) -> ApiResult<Arc<minidsp::client::MiniDspClient>> {
    state
        .get_device(id)
        .await
        .ok_or_else(|| ApiError::not_found())
}

pub fn router(state: Arc<DaemonState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/devices", get(list_devices))
        .route(
            "/devices/{id}",
            get(device_get_or_ws).post(set_device_status),
        )
        .route("/devices/{id}/config", post(write_config).get(read_config))
        .route("/devices/{id}/volume/{direction}", post(volume_direction))
        .route("/devices/{id}/source/{source}", post(set_source))
        .route("/devices/{id}/preset/{preset}", post(set_preset))
        .route("/devices/{id}/play", post(play_device))
        .route("/sync", post(sync_clock))
        .route("/devices/{id}/display", get(get_display).post(set_display))
        .route("/devices/{id}/dre", get(get_dre).post(set_dre))
        .route(
            "/devices/{id}/measurement-mode",
            post(enter_measurement_mode),
        )
        .route(
            "/devices/{id}/measurement-mode/restore",
            post(restore_measurement_mode),
        )
        .route("/devices/{id}/tokens", get(get_tokens))
        .route("/devices/{id}/preset/copy", post(copy_preset))
        .route("/devices/{id}/ws", get(ws_raw_bridge))
        .route("/openapi.json", get(openapi))
        .route("/llms.txt", get(llms_txt))
        .fallback(fallback)
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
    let is_ws = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));

    if is_ws {
        let ws = WebSocketUpgrade::from_request(req, &state)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let poll = params.poll.unwrap_or(false);
        let levels = params.levels.unwrap_or(false);
        return Ok(ws
            .on_upgrade(move |socket| ws_state_levels(socket, client, poll, levels))
            .into_response());
    }

    // Otherwise return JSON status — uses StatusSummary's Serialize derive directly
    // for correct f32 precision and skip_serializing_if on dirac
    let status = client
        .get_status_summary()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(status).into_response())
}

/// WebSocket: state + level streaming (compatibility endpoint on /devices/{id}).
async fn ws_state_levels(
    mut socket: WebSocket,
    client: Arc<minidsp::client::MiniDspClient>,
    poll: bool,
    levels: bool,
) {
    let interval_ms = if levels {
        250
    } else if poll {
        1000
    } else {
        5000
    };
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(interval_ms));

    // Send initial status immediately so clients don't wait for first tick
    if let Ok(status) = client.get_status_summary().await {
        let json = serde_json::to_string(&status).unwrap_or_default();
        if socket.send(Message::Text(json.into())).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            // Send status on each tick
            _ = tick.tick() => {
                match client.get_status_summary().await {
                    Ok(status) => {
                        // Use serde_json::to_string for correct f32 precision and dirac skip_serializing_if
                        let json = serde_json::to_string(&status).unwrap_or_default();
                        if socket.send(Message::Text(json.into())).await.is_err() {
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

async fn ws_raw_handler(mut socket: WebSocket, client: Arc<minidsp::client::MiniDspClient>) {
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
    match client
        .raw_command(
            minidsp_protocol::commands::read_flash(0xFF, 0xE0, 1),
            minidsp::queue::Priority::Normal,
        )
        .await
    {
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
    let mut ids: Vec<_> = devices.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let Some(client) = devices.get(&id) else {
            continue;
        };
        let info = client.device_info();
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
) -> ApiResult<impl IntoResponse> {
    let client = get_device_or_404(&state, id).await?;

    if let Some(volume) = body.volume {
        client
            .set_volume_db(volume)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }
    if let Some(mute) = body.mute {
        client
            .set_mute(mute)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }
    if let Some(preset) = body.preset {
        client
            .set_config(preset)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }
    if let Some(source) = &body.source {
        let src = minidsp_protocol::source::Source::from_str_pascal(source)
            .or_else(|| minidsp_protocol::source::Source::from_str_lowercase(source))
            .ok_or_else(|| ApiError::bad_request(format!("unknown source '{source}'")))?;
        let idx = client
            .profile()
            .sources
            .index_for_source(src)
            .ok_or_else(|| ApiError::bad_request("source not available on this device"))?;
        client
            .set_source(idx)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }
    if let Some(dirac) = body.dirac {
        match client.set_dirac_bypass(!dirac).await {
            Ok(()) => {}
            Err(minidsp::client::ClientError::Unsupported) => {}
            Err(e) => return Err(ApiError::internal(e.to_string())),
        }
    }

    let master = client
        .get_master_status()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let source_name = client
        .profile()
        .sources
        .source_for_index(master.source)
        .map(|s| s.pascal_case().to_string());
    let dirac = read_dirac_state(&client).await;
    Ok(Json(MasterStatusResponseJson {
        preset: Some(master.preset),
        source: source_name,
        volume: Some(master.volume_db()),
        mute: Some(master.is_muted()),
        dirac,
    }))
}

async fn sync_clock(Json(body): Json<SyncRequest>) -> impl IntoResponse {
    let server_time_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or_default();
    Json(SyncResponse {
        client_time_us: body.client_time_us,
        server_time_us,
    })
}

async fn play_device(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> ApiResult<Json<serde_json::Value>> {
    let client = get_device_or_404(&state, id).await?;
    if !client.profile().has_usb_audio {
        return Err(ApiError::bad_request(
            "device does not support USB audio playback",
        ));
    }
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "desktop playback backend not implemented",
    ))
}

async fn volume_direction(
    State(state): State<Arc<DaemonState>>,
    Path((id, direction)): Path<(usize, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let master = client
        .get_master_status()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match direction.as_str() {
        "up" => {
            let new_vol = master.volume.saturating_sub(1);
            client
                .set_volume(new_vol)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        "down" => {
            let new_vol = master.volume.saturating_add(1);
            client
                .set_volume(new_vol)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        "toggle_mute" => {
            client
                .set_mute(!master.is_muted())
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    }

    let updated = client
        .get_master_status()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_name = client
        .profile()
        .sources
        .source_for_index(updated.source)
        .map(|s| s.pascal_case().to_string());
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
        .or_else(|| minidsp_protocol::source::Source::from_str_pascal(&source))
        .ok_or(StatusCode::BAD_REQUEST)?;
    let idx = client
        .profile()
        .sources
        .index_for_source(src)
        .ok_or(StatusCode::BAD_REQUEST)?;
    client
        .set_source(idx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let master = client
        .get_master_status()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_name = client
        .profile()
        .sources
        .source_for_index(master.source)
        .map(|s| s.pascal_case().to_string());
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
    client
        .set_config(preset)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let master = client
        .get_master_status()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_name = client
        .profile()
        .sources
        .source_for_index(master.source)
        .map(|s| s.pascal_case().to_string());
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
            client
                .set_config(preset)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        if let Some(volume) = ms.volume {
            client
                .set_volume_db(volume)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        if let Some(mute) = ms.mute {
            client
                .set_mute(mute)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        if let Some(source) = &ms.source {
            let src = minidsp_protocol::source::Source::from_str_pascal(source)
                .or_else(|| minidsp_protocol::source::Source::from_str_lowercase(source))
                .ok_or(StatusCode::BAD_REQUEST)?;
            let idx = client
                .profile()
                .sources
                .index_for_source(src)
                .ok_or(StatusCode::BAD_REQUEST)?;
            client
                .set_source(idx)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        if let Some(dirac) = ms.dirac {
            match client.set_dirac_bypass(!dirac).await {
                Ok(()) => {}
                Err(minidsp::client::ClientError::Unsupported) => {} // expected for non-Dirac devices
                Err(e) => return Err(client_error_to_status(e)),
            }
        }
    }

    if let Some(inputs) = body.inputs {
        let table = client
            .profile()
            .dsp_table
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        for input in inputs {
            if let Some(input_ch) = table.inputs.get(input.index as usize) {
                if let Some(peqs) = input.peq {
                    for peq in peqs {
                        if let Some(band) = input_ch.peq.get(peq.index as usize) {
                            if let Some(coeff) = peq.coeff {
                                let coeffs = [coeff.b0, coeff.b1, coeff.b2, coeff.a1, coeff.a2];
                                client
                                    .write_biquad(band.addr, &coeffs)
                                    .await
                                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                            }
                            if let Some(bypass) = peq.bypass {
                                client
                                    .bypass_filter(band.addr, bypass)
                                    .await
                                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                            }
                        }
                    }
                }
                // Per-channel gain (input channels have Option<u16> addresses)
                if let Some(gain) = input.gain {
                    if let Some(gain_addr) = input_ch.gain {
                        client
                            .set_channel_gain(gain_addr, gain)
                            .await
                            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                    }
                }
                // Per-channel mute
                if let Some(mute) = input.mute {
                    if let Some(mute_addr) = input_ch.mute {
                        client
                            .set_channel_mute(mute_addr, mute)
                            .await
                            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                    }
                }
            }
        }
    }

    if let Some(outputs) = body.outputs {
        let table = client
            .profile()
            .dsp_table
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        for output in outputs {
            if let Some(output_ch) = table.outputs.get(output.index as usize) {
                if let Some(peqs) = output.peq {
                    for peq in peqs {
                        if let Some(band) = output_ch.peq.get(peq.index as usize) {
                            if let Some(coeff) = peq.coeff {
                                let coeffs = [coeff.b0, coeff.b1, coeff.b2, coeff.a1, coeff.a2];
                                client
                                    .write_biquad(band.addr, &coeffs)
                                    .await
                                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                            }
                            if let Some(bypass) = peq.bypass {
                                client
                                    .bypass_filter(band.addr, bypass)
                                    .await
                                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                            }
                        }
                    }
                }
                // Per-channel gain (output channels always have gain address)
                if let Some(gain) = output.gain {
                    client
                        .set_channel_gain(output_ch.gain, gain)
                        .await
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                }
                // Per-channel mute
                if let Some(mute) = output.mute {
                    client
                        .set_channel_mute(output_ch.mute, mute)
                        .await
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                }
                // Delay (Duration format: secs + nanos → sample count)
                if let Some(delay) = output.delay {
                    if let Some(delay_addr) = output_ch.delay {
                        let sample_rate = client.profile().sample_rate as f32;
                        let samples = (delay.as_secs_f32() * sample_rate)
                            .round()
                            .clamp(0.0, 48000.0);
                        client
                            .set_channel_delay_samples(delay_addr, samples as u32)
                            .await
                            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                    }
                }
                // Polarity inversion (dialect-aware encoding)
                if let Some(invert) = output.invert {
                    if let Some(polarity_addr) = output_ch.polarity {
                        client
                            .set_invert(polarity_addr, invert)
                            .await
                            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    let table = client
        .profile()
        .dsp_table
        .ok_or(StatusCode::NOT_IMPLEMENTED)?;
    let config = client
        .read_device_config()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let inputs = config
        .inputs
        .into_iter()
        .map(|input| InputConfigJson {
            index: input.index,
            peq: Some(
                input
                    .peq
                    .into_iter()
                    .map(|peq| PeqConfigJson {
                        index: peq.index,
                        coeff: Some(BiquadCoeffJson {
                            b0: peq.coeff.b0,
                            b1: peq.coeff.b1,
                            b2: peq.coeff.b2,
                            a1: peq.coeff.a1,
                            a2: peq.coeff.a2,
                        }),
                        bypass: Some(peq.bypass),
                    })
                    .collect(),
            ),
            gain: input.gain,
            mute: input.mute,
        })
        .collect::<Vec<_>>();

    let mut outputs = Vec::new();
    for (idx, output) in config.outputs.into_iter().enumerate() {
        let invert = if let Some(ch) = table.outputs.get(idx) {
            match ch.polarity {
                Some(addr) => client.read_channel_invert(addr).await.ok(),
                None => None,
            }
        } else {
            None
        };
        outputs.push(OutputConfigJson {
            index: output.index,
            peq: Some(
                output
                    .peq
                    .into_iter()
                    .map(|peq| PeqConfigJson {
                        index: peq.index,
                        coeff: Some(BiquadCoeffJson {
                            b0: peq.coeff.b0,
                            b1: peq.coeff.b1,
                            b2: peq.coeff.b2,
                            a1: peq.coeff.a1,
                            a2: peq.coeff.a2,
                        }),
                        bypass: Some(peq.bypass),
                    })
                    .collect(),
            ),
            gain: Some(output.gain),
            mute: Some(output.mute),
            delay: Some(std::time::Duration::from_secs_f32(output.delay / 1000.0)),
            invert,
        });
    }

    Ok(Json(serde_json::json!({
        "inputs": inputs,
        "outputs": outputs,
    }))
    .into_response())
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
    let resp = client
        .raw_command(
            minidsp_protocol::commands::read_flash(0xFF, 0xE8, 2),
            minidsp::queue::Priority::Normal,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match resp {
        minidsp_protocol::commands::Response::FlashData(data) if data.len() >= 2 => {
            Ok(Json(serde_json::json!({
                "idle_time": data[0],
                "brightness": data[1],
            }))
            .into_response())
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
        client
            .raw_command(
                minidsp_protocol::commands::set_display_brightness(brightness as u8),
                minidsp::queue::Priority::High,
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if let Some(idle_time) = body.get("idle_time").and_then(|v| v.as_u64()) {
        client
            .raw_command(
                minidsp_protocol::commands::set_display_idle_time(idle_time as u8),
                minidsp::queue::Priority::High,
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    let resp = client
        .raw_command(
            minidsp_protocol::commands::read_flash(0xFF, 0xED, 1),
            minidsp::queue::Priority::Normal,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
        client
            .raw_command(
                minidsp_protocol::commands::set_dre(enabled),
                minidsp::queue::Priority::High,
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(StatusCode::OK)
}

async fn get_tokens(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let resp = client
        .raw_command(
            minidsp_protocol::commands::read_flash(0xFF, 0xC8, 16),
            minidsp::queue::Priority::Normal,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
    client
        .raw_command(
            minidsp_protocol::commands::copy_preset(true),
            minidsp::queue::Priority::Critical,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}

async fn enter_measurement_mode(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    client
        .enter_measurement_mode()
        .await
        .map_err(client_error_to_status)?;
    let saved = client
        .saved_measurement_state()
        .await
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    state.set_saved_peq(id, saved).await;
    Ok(Json(
        serde_json::json!({ "status": "measurement_mode_active" }),
    ))
}

async fn restore_measurement_mode(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let saved = state.get_saved_peq(id).await.ok_or(StatusCode::NOT_FOUND)?;
    client
        .restore_measurement_mode_with(&saved)
        .await
        .map_err(client_error_to_status)?;
    client.set_saved_measurement_state(None).await;
    state.clear_saved_peq(id).await;
    Ok(Json(
        serde_json::json!({ "status": "measurement_mode_restored" }),
    ))
}

/// Map ClientError to HTTP status code, including 503 for busy.
fn client_error_to_status(e: minidsp::client::ClientError) -> StatusCode {
    match e {
        minidsp::client::ClientError::Busy => StatusCode::SERVICE_UNAVAILABLE,
        minidsp::client::ClientError::Transport(minidsp::transport::TransportError::Busy) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        minidsp::client::ClientError::Unsupported => StatusCode::NOT_IMPLEMENTED,
        minidsp::client::ClientError::Timeout => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Axum fallback handler — returns JSON errors for unmatched routes.
async fn fallback() -> impl IntoResponse {
    ApiError::new(StatusCode::NOT_FOUND, "not found")
}

async fn openapi() -> impl IntoResponse {
    let spec = serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "minidsp-open API",
            "description": "MiniDSP controller HTTP API. Drop-in replacement for minidsp-rs.",
            "version": "0.1.0",
            "license": { "name": "MIT" }
        },
        "servers": [
            { "url": "http://localhost:5380", "description": "Default daemon" }
        ],
        "paths": {
            "/devices": {
                "get": {
                    "summary": "List connected devices",
                    "operationId": "listDevices",
                    "responses": {
                        "200": {
                            "description": "Array of connected MiniDSP devices",
                            "content": { "application/json": { "schema": { "type": "array" } } }
                        }
                    }
                }
            },
            "/devices/{id}": {
                "get": {
                    "summary": "Get device status (or WebSocket upgrade for streaming)",
                    "operationId": "getDeviceStatus",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "integer" } },
                        { "name": "levels", "in": "query", "schema": { "type": "boolean" } },
                        { "name": "poll", "in": "query", "schema": { "type": "boolean" } }
                    ],
                    "responses": {
                        "200": { "description": "Device status summary" },
                        "101": { "description": "WebSocket upgrade for state/level streaming" },
                        "404": { "description": "Device not found" }
                    }
                },
                "post": {
                    "summary": "Set master status (volume, mute, preset, source, dirac)",
                    "operationId": "setDeviceStatus",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "integer" } }
                    ],
                    "requestBody": {
                        "content": { "application/json": { "schema": { "type": "object" } } }
                    },
                    "responses": {
                        "200": { "description": "Updated master status" },
                        "503": { "description": "Device busy (queue full)" }
                    }
                }
            },
            "/devices/{id}/config": {
                "get": {
                    "summary": "Read full device config (PEQ coefficients, gains, delays)",
                    "operationId": "readConfig",
                    "responses": { "200": { "description": "Full device config" } }
                },
                "post": {
                    "summary": "Write device config (PEQ, gains)",
                    "operationId": "writeConfig",
                    "responses": { "200": { "description": "Config applied" } }
                }
            },
            "/devices/{id}/volume/{direction}": {
                "post": {
                    "summary": "Volume up/down/toggle_mute",
                    "operationId": "volumeDirection",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "integer" } },
                        { "name": "direction", "in": "path", "required": true, "schema": { "type": "string", "enum": ["up", "down", "toggle_mute"] } }
                    ],
                    "responses": { "200": { "description": "Updated status" } }
                }
            },
            "/devices/{id}/source/{source}": {
                "post": {
                    "summary": "Set input source",
                    "operationId": "setSource",
                    "responses": { "200": { "description": "Updated status" } }
                }
            },
            "/devices/{id}/preset/{preset}": {
                "post": {
                    "summary": "Switch preset (0-3)",
                    "operationId": "setPreset",
                    "responses": { "200": { "description": "Updated status" } }
                }
            },
            "/devices/{id}/display": {
                "get": { "summary": "Get display settings", "responses": { "200": { "description": "Display state" } } },
                "post": { "summary": "Set display brightness/idle time", "responses": { "200": { "description": "Updated" } } }
            },
            "/devices/{id}/dre": {
                "get": { "summary": "Get DRE state", "responses": { "200": { "description": "DRE state" } } },
                "post": { "summary": "Set DRE enabled/disabled", "responses": { "200": { "description": "Updated" } } }
            },
            "/devices/{id}/measurement-mode": {
                "post": {
                    "summary": "Enter measurement mode (bypass all PEQ)",
                    "operationId": "enterMeasurementMode",
                    "responses": {
                        "200": { "description": "Measurement mode active, PEQ state saved" },
                        "501": { "description": "Device does not support PEQ" }
                    }
                }
            },
            "/devices/{id}/measurement-mode/restore": {
                "post": {
                    "summary": "Restore PEQ state from measurement mode",
                    "operationId": "restoreMeasurementMode",
                    "responses": { "200": { "description": "PEQ state restored" } }
                }
            },
            "/devices/{id}/tokens": {
                "get": { "summary": "Read modification tokens", "responses": { "200": { "description": "Token values" } } }
            },
            "/devices/{id}/preset/copy": {
                "post": { "summary": "Copy current preset to flash", "responses": { "200": { "description": "Preset copied" } } }
            },
            "/devices/{id}/ws": {
                "get": {
                    "summary": "WebSocket raw frame bridge",
                    "operationId": "wsRawBridge",
                    "responses": { "101": { "description": "WebSocket upgrade" } }
                }
            },
            "/openapi.json": {
                "get": { "summary": "This OpenAPI specification", "responses": { "200": { "description": "OpenAPI 3.0 JSON" } } }
            },
            "/llms.txt": {
                "get": { "summary": "LLM discovery file", "responses": { "200": { "description": "Plain text LLM info" } } }
            }
        }
    });
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::to_string_pretty(&spec).unwrap(),
    )
}

async fn llms_txt() -> impl IntoResponse {
    (
        StatusCode::OK,
        "# minidsp-open\n\nMiniDSP controller HTTP API.\nSee /openapi.json for endpoints.\n",
    )
}
