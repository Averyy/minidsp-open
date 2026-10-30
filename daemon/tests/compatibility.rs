//! Host-only interoperability regressions. Original fake transport; no upstream code.
use futures_util::{SinkExt, StreamExt};
use minidsp::{
    client::MiniDspClient,
    transport::{Transport, TransportError},
};
use minidsp_daemon::{http::routes, state::DaemonState};
use minidsp_protocol::{commands::*, packet};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

struct MemoryListener(tokio::sync::mpsc::UnboundedReceiver<DuplexStream>);
impl axum::serve::Listener for MemoryListener {
    type Io = DuplexStream;
    type Addr = ();
    async fn accept(&mut self) -> (Self::Io, ()) {
        match self.0.recv().await {
            Some(stream) => (stream, ()),
            None => std::future::pending().await,
        }
    }
    fn local_addr(&self) -> std::io::Result<()> {
        Ok(())
    }
}
fn connection(sender: &tokio::sync::mpsc::UnboundedSender<DuplexStream>) -> DuplexStream {
    let (client, server) = tokio::io::duplex(65536);
    sender.send(server).unwrap();
    client
}
fn api(
    state: Arc<DaemonState>,
    origin: Option<&str>,
) -> (
    tokio::sync::mpsc::UnboundedSender<DuplexStream>,
    tokio::task::JoinHandle<()>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let app = routes::router_with_origins(state, origin).unwrap();
    let server = tokio::spawn(async move {
        axum::serve(MemoryListener(receiver), app).await.unwrap();
    });
    (sender, server)
}

struct FakeTransport {
    pending: Mutex<VecDeque<Vec<u8>>>,
    serial: u16,
    volume: Mutex<u8>,
    /// Shared with the test so the next master-status read can be failed.
    fail_status: Arc<std::sync::atomic::AtomicBool>,
}
impl Transport for FakeTransport {
    fn send_frame(
        &self,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let response = match payload[0] {
            CMD_READ_HARDWARE_ID => vec![CMD_READ_HARDWARE_ID, 1, 13, 10],
            CMD_READ_FLASH => match payload[2] {
                0xa1 => vec![CMD_READ_FLASH, 100],
                0xfe => {
                    let [a, b] = self.serial.to_be_bytes();
                    vec![CMD_READ_FLASH, a, b]
                }
                0xd8 if self
                    .fail_status
                    .swap(false, std::sync::atomic::Ordering::SeqCst) =>
                {
                    vec![RESP_ERROR]
                }
                0xd8 => vec![CMD_READ_FLASH, 0, 0, *self.volume.lock().unwrap(), 0],
                _ => {
                    let mut r = vec![CMD_READ_FLASH];
                    r.resize(1 + payload[3] as usize, 0);
                    r
                }
            },
            CMD_READ_FLOATS => {
                let mut r = vec![CMD_READ_FLOATS, payload[1], payload[2]];
                for _ in 0..payload[3] {
                    r.extend_from_slice(&0f32.to_le_bytes());
                }
                r
            }
            CMD_SET_VOLUME => {
                *self.volume.lock().unwrap() = payload[1];
                vec![RESP_OK]
            }
            _ => payload.to_vec(),
        };
        let mut pending = self.pending.lock().unwrap();
        assert!(pending.is_empty(), "transport exchanges must be serialized");
        pending.push_back(response);
        Box::pin(async { Ok(()) })
    }
    fn recv_frame(
        &self,
        _: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
        Box::pin(async {
            tokio::task::yield_now().await;
            self.pending
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(TransportError::Timeout)
        })
    }
    fn is_connected(&self) -> bool {
        true
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
    fn url(&self) -> String {
        format!("usb:fake-{}", self.serial)
    }
}
async fn state() -> Arc<DaemonState> {
    state_with(Arc::new(DaemonState::new())).await
}
async fn state_with(state: Arc<DaemonState>) -> Arc<DaemonState> {
    for (index, serial) in [12345, 23456].into_iter().enumerate() {
        let client = MiniDspClient::connect(Box::new(FakeTransport {
            pending: Mutex::new(VecDeque::new()),
            serial,
            volume: Mutex::new(40),
            fail_status: Default::default(),
        }))
        .await
        .unwrap();
        // No persistence or Device Console cache reads in these tests.
        client.bind_volume_settings(state.settings.clone());
        state.devices.write().await.insert(index, Arc::new(client));
    }
    state
}
async fn http(
    sender: &tokio::sync::mpsc::UnboundedSender<DuplexStream>,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String, Vec<u8>) {
    let mut stream = connection(sender);
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut data = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut data),
    )
    .await
    .unwrap()
    .unwrap();
    let split = data.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
    let headers = String::from_utf8(data[..split].to_vec()).unwrap();
    let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, headers, data[split + 4..].to_vec())
}

// Upstream 8e987f9: daemon/src/config.rs:59-72 and daemon/src/tcp/mod.rs:185-215.
#[tokio::test]
async fn configured_multi_device_binding_prefers_serial() {
    let state = state().await;
    assert_eq!(
        state
            .bridge_device(Some(0), Some(923456))
            .await
            .unwrap()
            .serial(),
        923456
    );
    assert_eq!(
        state.bridge_device(Some(1), None).await.unwrap().serial(),
        923456
    );
    assert_eq!(
        state.bridge_device(None, None).await.unwrap().serial(),
        912345
    );
    assert!(state.bridge_device(None, Some(999999)).await.is_none());
}

// Upstream 8e987f9: daemon/src/http/mod.rs:131-250, minidsp/src/model.rs:12-20.
// ezBEQ bd981bf: ezbeq/minidsp.py:1068-1090 requires complete nonempty master state.
#[tokio::test]
async fn http_and_state_websocket_keep_ezbeq_shapes_and_json_errors() {
    let state = state().await;
    let (sender, server) = api(state.clone(), Some("https://example.com"));
    let (status, headers, body) = http(&sender, "GET", "/devices", "").await;
    assert_eq!(status, 200);
    // Native reads have no Origin to echo, even with a configured browser UI.
    assert!(
        !headers
            .to_ascii_lowercase()
            .contains("access-control-allow-origin:")
    );
    let devices: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(devices[1]["version"]["serial"], 923456);
    let (_, _, body) = http(&sender, "GET", "/devices/0", "").await;
    let get: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(get["master"].get("dirac").is_none());
    assert_eq!(get["input_levels"].as_array().unwrap().len(), 2);
    assert_eq!(get["output_levels"].as_array().unwrap().len(), 4);
    let (status, _, body) = http(&sender, "POST", "/devices/0", r#"{"volume":-21}"#).await;
    assert_eq!(status, 200);
    let post: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(post["volume"], -21.0);
    assert_eq!(post.get("dirac"), Some(&serde_json::Value::Null));
    let (mut ws, _) = tokio_tungstenite::client_async(
        "ws://localhost/devices/0?poll=false&levels=anything",
        connection(&sender),
    )
    .await
    .unwrap();
    for _ in 0..2 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let status: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
        assert_eq!(status["master"]["preset"], 0);
        assert_eq!(status["master"]["volume"], -21.0);
        assert!(status["input_levels"].is_array());
    }
    ws.close(None).await.unwrap();
    state.remove_device(0).await;
    for method in ["GET", "POST"] {
        let (status, headers, body) = http(&sender, method, "/devices/0", "{}").await;
        assert_eq!(status, 404);
        assert!(headers.to_ascii_lowercase().contains("application/json"));
        assert!(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"].is_string());
    }
    server.abort();
}

// Upstream 8e987f9: minidsp/src/transport/net/codec.rs:10-12, 126-172;
// daemon/src/tcp/mod.rs:143-177 accepts multiple clients, with no single-client rule.
#[tokio::test]
async fn tcp_clients_and_http_share_queue_without_cross_wiring() {
    let state = state().await;
    let (mut first, server_first) = tokio::io::duplex(4096);
    let (mut second, server_second) = tokio::io::duplex(4096);
    let mutex = Arc::new(tokio::sync::Mutex::new(()));
    let peer = "127.0.0.1:1234".parse().unwrap();
    let first_server = tokio::spawn(minidsp_daemon::tcp::serve_connection(
        server_first,
        peer,
        state.clone(),
        mutex.clone(),
        Some(1),
        None,
    ));
    let second_server = tokio::spawn(minidsp_daemon::tcp::serve_connection(
        server_second,
        peer,
        state.clone(),
        mutex,
        Some(1),
        None,
    ));
    let (sender, server) = api(state.clone(), None);
    let direct = async {
        for volume in [-21.0, -22.0, -20.0] {
            let (status, _, body) = http(
                &sender,
                "POST",
                "/devices/1",
                &format!(r#"{{"volume":{volume}}}"#),
            )
            .await;
            assert_eq!(status, 200);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["volume"],
                volume
            );
            assert_eq!(http(&sender, "GET", "/devices/1", "").await.0, 200);
        }
    };
    let first_work = async {
        for tag in 0..10 {
            let frame = packet::encode_frame(&[0xe9, tag]);
            first.write_all(&frame[..1]).await.unwrap();
            tokio::task::yield_now().await;
            first.write_all(&frame[1..]).await.unwrap();
            let mut response = [0; 3];
            first.read_exact(&mut response).await.unwrap();
            assert_eq!(response, [3, 0xe9, tag]);
        }
    };
    let second_work = async {
        for tag in 100..110 {
            let mut frame = packet::encode_frame(&[0xe9, tag]);
            frame.resize(64, 0);
            second.write_all(&frame).await.unwrap();
            let mut response = [0; 64];
            second.read_exact(&mut response).await.unwrap();
            assert_eq!(&response[..3], &[3, 0xe9, tag]);
            assert!(
                response[3..].iter().all(|v| *v == 0),
                "responses have no checksum, including padded mode"
            );
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(direct, first_work, second_work);
    })
    .await
    .unwrap();
    server.abort();
    first_server.abort();
    second_server.abort();
}

#[tokio::test]
async fn volume_ceiling_settings_absolute_relative_config_and_persistence() {
    let dir = std::env::temp_dir().join(format!("minidsp-volume-settings-{}", std::process::id()));
    let cache = dir.join("shadow-cache.json");
    let _ = std::fs::remove_dir_all(&dir);
    let state = state_with(Arc::new(DaemonState::with_cache_path(cache.clone()))).await;
    let (sender, server) = api(state.clone(), None);
    let (status, _, body) = http(&sender, "GET", "/settings", "").await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"maxVolumeDb":null})
    );
    for invalid in [
        r#"{}"#,
        r#"{"maxVolumeDb":-20.25}"#,
        r#"{"maxVolumeDb":-127.5}"#,
        r#"{"maxVolumeDb":1}"#,
    ] {
        assert_eq!(http(&sender, "POST", "/settings", invalid).await.0, 400);
    }
    assert_eq!(
        http(&sender, "POST", "/settings", r#"{"maxVolumeDb":-30.5}"#)
            .await
            .0,
        200
    );
    // Every connected device is lowered immediately, before any subsequent write.
    for id in [0, 1] {
        let (_, _, body) = http(&sender, "GET", &format!("/devices/{id}"), "").await;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["master"]["volume"],
            -30.5
        );
    }
    for (path, body, expected) in [
        ("/devices/0", r#"{"volume":0}"#, -30.5),
        ("/devices/0/volume/up", "", -30.5),
        ("/devices/0/volume/down", "", -31.0),
        ("/devices/0/volume/up", "", -30.5),
        ("/devices/0", r#"{"volume":-40}"#, -40.0),
    ] {
        let (status, _, body) = http(&sender, "POST", path, body).await;
        assert_eq!(status, 200);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["volume"],
            expected
        );
    }
    assert_eq!(
        http(
            &sender,
            "POST",
            "/devices/0/config",
            r#"{"master_status":{"volume":0}}"#
        )
        .await
        .0,
        200
    );
    let (_, _, body) = http(&sender, "GET", "/devices/0", "").await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["master"]["volume"],
        -30.5
    );
    let (status, _, body) = http(
        &sender,
        "POST",
        "/devices/0/rattle",
        r#"{"volumesDb":[-40,-30]}"#,
    )
    .await;
    assert_eq!(status, 400);
    assert!(String::from_utf8(body).unwrap().contains("-30.5"));
    let restarted = DaemonState::with_cache_path(cache.clone());
    restarted.load_settings(None).await.unwrap();
    assert_eq!(restarted.settings.read().await.max_volume_db(), Some(-30.5));
    restarted.load_settings(Some(-25.0)).await.unwrap();
    assert_eq!(restarted.settings.read().await.max_volume_db(), Some(-25.0));
    assert!(restarted.load_settings(Some(-25.1)).await.is_err());
    assert_eq!(
        http(&sender, "POST", "/settings", r#"{"maxVolumeDb":null}"#)
            .await
            .0,
        200
    );
    let restarted = DaemonState::with_cache_path(cache);
    restarted.load_settings(None).await.unwrap();
    assert_eq!(restarted.settings.read().await.max_volume_db(), None);
    let (status, _, body) = http(&sender, "POST", "/devices/0", r#"{"volume":0}"#).await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["volume"],
        0.0
    );
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn failed_ceiling_enforcement_registers_the_device_and_retries() {
    let dir = std::env::temp_dir().join(format!("minidsp-ceiling-retry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = Arc::new(DaemonState::with_cache_path(dir.join("shadow-cache.json")));
    *state.settings.write().await = minidsp_protocol::settings::Settings::new(Some(-20.0)).unwrap();
    let fail_status = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = MiniDspClient::connect(Box::new(FakeTransport {
        pending: Mutex::new(VecDeque::new()),
        serial: 34567,
        // 0 dB, louder than the -20 dB ceiling.
        volume: Mutex::new(0),
        fail_status: fail_status.clone(),
    }))
    .await
    .unwrap();
    fail_status.store(true, std::sync::atomic::Ordering::SeqCst);
    let client = Arc::new(client);
    state.add_device(0, client.clone()).await;
    assert!(state.devices.read().await.contains_key(&0));
    assert_eq!(client.get_master_status().await.unwrap().volume, 0);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while client.get_master_status().await.map(|m| m.volume).ok() != Some(40) {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the ceiling guard lowers the volume after the failed first attempt");

    // A ceiling saved while the device wasn't listed yet (as when POST
    // /settings races registration) still reaches it.
    *state.settings.write().await = minidsp_protocol::settings::Settings::new(Some(-30.0)).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while client.get_master_status().await.map(|m| m.volume).ok() != Some(60) {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the ceiling guard applies a settings change it didn't see enforced");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn tcp_and_raw_websocket_volume_writes_obey_live_ceiling() {
    let state = state().await;
    *state.settings.write().await = minidsp_protocol::settings::Settings::new(Some(-35.5)).unwrap();
    let client = state.get_device(0).await.unwrap();
    let (mut tcp, stream) = tokio::io::duplex(4096);
    let task = tokio::spawn(minidsp_daemon::tcp::serve_connection(
        stream,
        "127.0.0.1:1234".parse().unwrap(),
        state.clone(),
        Arc::new(tokio::sync::Mutex::new(())),
        Some(0),
        None,
    ));
    tcp.write_all(&packet::encode_frame(&set_volume(0)))
        .await
        .unwrap();
    let mut reply = [0; 1];
    tcp.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply, [RESP_OK]);
    assert_eq!(client.get_master_status().await.unwrap().volume, 71);
    // Quieter writes pass through unchanged.
    tcp.write_all(&packet::encode_frame(&set_volume(100)))
        .await
        .unwrap();
    tcp.read_exact(&mut reply).await.unwrap();
    assert_eq!(client.get_master_status().await.unwrap().volume, 100);
    let (sender, server) = api(state.clone(), None);
    let (mut ws, _) =
        tokio_tungstenite::client_async("ws://localhost/devices/0/ws", connection(&sender))
            .await
            .unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Binary(
        packet::encode_frame(&set_volume(0)),
    ))
    .await
    .unwrap();
    let response = ws.next().await.unwrap().unwrap();
    assert_eq!(
        packet::decode_frame(&response.into_data()).unwrap(),
        [RESP_OK]
    );
    assert_eq!(client.get_master_status().await.unwrap().volume, 71);
    ws.close(None).await.unwrap();
    server.abort();
    task.abort();
}
