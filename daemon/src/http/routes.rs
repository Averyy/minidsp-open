//! Axum route definitions and handlers.

use crate::http::json::*;
use crate::state::DaemonState;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use std::sync::Arc;

pub fn router(state: Arc<DaemonState>) -> Router {
    Router::new()
        .route("/devices", get(list_devices))
        .route("/devices/{id}", get(get_device_status).post(set_device_status))
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
        .route("/openapi.json", get(openapi))
        .route("/llms.txt", get(llms_txt))
        .with_state(state)
}

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

async fn get_device_status(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<usize>,
) -> Result<impl IntoResponse, StatusCode> {
    let client = state.get_device(id).await.ok_or(StatusCode::NOT_FOUND)?;
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
    }))
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

    // Return updated status (POST response includes null fields)
    let master = client.get_master_status().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let source_name = client
        .profile()
        .sources
        .source_for_index(master.source)
        .map(|s| s.pascal_case().to_string());
    let dirac = if client.profile().has_dirac { Some(false) } else { None };
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
            let new_vol = master.volume.saturating_sub(1); // lower value = louder
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
    let dirac = if client.profile().has_dirac { Some(false) } else { None };
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
    let dirac = if client.profile().has_dirac { Some(false) } else { None };
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
    let dirac = if client.profile().has_dirac { Some(false) } else { None };
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

    // Apply master status changes
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

    // Apply input PEQ changes
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

    // Apply output PEQ changes
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
    State(_state): State<Arc<DaemonState>>,
    Path(_id): Path<usize>,
) -> impl IntoResponse {
    // TODO: implement config read-back
    StatusCode::NOT_IMPLEMENTED
}

async fn get_display() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }
async fn set_display() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }
async fn get_dre() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }
async fn set_dre() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }
async fn enter_measurement_mode() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }
async fn restore_measurement_mode() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }
async fn get_tokens() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }
async fn copy_preset() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }
async fn openapi() -> impl IntoResponse { StatusCode::NOT_IMPLEMENTED }

async fn llms_txt() -> impl IntoResponse {
    (StatusCode::OK, "# minidsp-open\n\nMiniDSP controller HTTP API.\nSee /openapi.json for endpoints.\n")
}
