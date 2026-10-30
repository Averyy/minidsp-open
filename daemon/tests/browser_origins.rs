//! Exercise the actual router without LAN sockets or connected hardware.
use minidsp_daemon::{http::routes, state::DaemonState};
use std::sync::Arc;
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

async fn request(configured: Option<&str>, method: &str, path: &str, headers: &str) -> String {
    let app = routes::router_with_origins(Arc::new(DaemonState::new()), configured).unwrap();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        axum::serve(MemoryListener(receiver), app).await.unwrap();
    });
    let (mut client, connection) = tokio::io::duplex(16384);
    sender.send(connection).unwrap();
    let connection_header = if headers.contains("Upgrade: websocket\r\n") {
        "Upgrade, close"
    } else {
        "close"
    };
    client.write_all(format!("{method} {path} HTTP/1.1\r\nHost: nas:5380\r\nConnection: {connection_header}\r\nContent-Length: 0\r\n{headers}\r\n").as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    server.abort();
    String::from_utf8(response).unwrap()
}

#[tokio::test]
async fn rejects_browser_mutations_before_content_type_or_route_handling() {
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        for origin in ["https://evil.example", "null", "http://rebind.example:5380"] {
            let response = request(
                None,
                method,
                "/devices/0",
                &format!("Origin: {origin}\r\nContent-Type: text/plain\r\n"),
            )
            .await;
            assert!(response.starts_with("HTTP/1.1 403"), "{response}");
            assert!(response.contains(r#"{"error":"origin not allowed"}"#));
            assert!(!response.contains("access-control-allow-origin:"));
        }
    }
    let response = request(
        None,
        "POST",
        "/devices/0",
        "Origin: http://nas:5380\r\nOrigin: https://evil.example\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 403"));
}

#[tokio::test]
async fn native_hostname_clients_and_local_browser_uis_reach_handlers() {
    for origin in [
        None,
        Some("http://nas:5380"),
        Some("http://192.168.1.2:8080"),
        Some("http://ui.local"),
        Some("http://[fd00::1]:8080"),
    ] {
        let headers = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        let response = request(None, "POST", "/devices/0", &headers).await;
        assert!(!response.starts_with("HTTP/1.1 403"), "{response}");
        if let Some(origin) = origin {
            assert!(response.contains(&format!("access-control-allow-origin: {origin}\r\n")));
        } else {
            assert!(!response.contains("access-control-allow-origin:"));
        }
    }
}

#[tokio::test]
async fn reads_and_preflights_echo_only_allowed_origins() {
    let response = request(None, "GET", "/devices", "Origin: https://evil.example\r\n").await;
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(!response.contains("access-control-allow-origin:"));
    for origin in ["http://nas:5380", "http://localhost:8080"] {
        let response = request(None, "OPTIONS", "/devices/0", &format!("Origin: {origin}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type, x-firmware-sha256\r\n")).await;
        assert!(response.starts_with("HTTP/1.1 204"), "{response}");
        assert!(response.contains(&format!("access-control-allow-origin: {origin}\r\n")));
        assert!(response.contains("access-control-allow-headers: content-type, x-firmware-sha256"));
    }
    let response = request(
        None,
        "OPTIONS",
        "/devices/0",
        "Origin: null\r\nAccess-Control-Request-Method: POST\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 403"));
    for configured in ["https://ui.example", "*"] {
        let response = request(
            Some(configured),
            "POST",
            "/devices/0",
            "Origin: https://ui.example\r\n",
        )
        .await;
        assert!(!response.starts_with("HTTP/1.1 403"));
        assert!(response.contains("access-control-allow-origin: https://ui.example"));
    }
}

#[tokio::test]
async fn both_websocket_routes_check_origin_before_upgrading() {
    for path in ["/devices/0", "/devices/0/ws"] {
        for origin in ["https://evil.example", "null"] {
            let response = request(None, "GET", path, &format!("Origin: {origin}\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n")).await;
            assert!(response.starts_with("HTTP/1.1 403"), "{response}");
            assert!(response.contains(r#"{"error":"origin not allowed"}"#));
        }
    }
}

#[tokio::test]
async fn settings_mutations_use_the_existing_origin_guard() {
    let response = request(
        None,
        "POST",
        "/settings",
        "Origin: https://evil.example\r\nContent-Type: application/json\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    let response = request(None, "GET", "/settings", "").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains(r#"{"maxVolumeDb":null}"#));
}
