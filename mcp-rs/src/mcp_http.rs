//! Streamable HTTP transport — MCP 2025-03-26 spec.
//!
//! POST /mcp    → JSON-RPC 2.0 request → JSON response
//! GET  /health → liveness probe
//!
//! SSE is deprecated per MCP 2025 spec; Streamable HTTP uses a single
//! endpoint for both requests and (optionally) server→client notifications.
//!
//! Security: defaults to 127.0.0.1 (loopback only). CORS is enabled via
//! `tower-http` so browser-based MCP clients can connect. No authentication
//! is provided — rely on network-layer isolation (loopback/firewall).

use std::sync::Arc;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;

use crate::mcp::{JsonRpcRawRequest, McpServer};
use crate::serial_engine::SharedEngine;

type SharedServer = Arc<Mutex<McpServer>>;

/// Run the Streamable HTTP server on the given host:port.
pub async fn run_http(
    engine: SharedEngine,
    bind_host: &str,
    bind_port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    // Spawn read loop: event-driven (epoll), with a short sleep after each
    // iteration to avoid a busy loop under data floods.
    let engine_read = engine.clone();
    let read_handle = tokio::spawn(async move {
        loop {
            let mut eng = engine_read.lock().await;
            eng.read_loop_iter().await;
            drop(eng);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    // Spawn watchdog: check hang/heartbeat every 2s.
    let engine_wd = engine.clone();
    let watchdog_handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            let mut eng = engine_wd.lock().await;
            eng.watchdog_once();
        }
    });

    // Register background task handles so stop() can clean them up.
    {
        let mut eng = engine.lock().await;
        eng.set_background_tasks(read_handle, watchdog_handle);
    }

    let server = Arc::new(Mutex::new(McpServer::new(engine.clone())));

    // CORS: allow all origins (this is a local debug tool, not a public API).
    let cors = CorsLayer::permissive();

    let app = Router::new()
        .route("/mcp", post(handle_mcp_post))
        .route("/health", get(handle_health))
        .layer(cors)
        .with_state(server);

    let addr = format!("{bind_host}:{bind_port}");
    tracing::info!("[embedded-debug-mcp] Streamable HTTP listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    // Shutdown: abort background tasks.
    {
        let mut eng = engine.lock().await;
        eng.stop().await;
    }

    Ok(())
}

/// POST /mcp — Main JSON-RPC endpoint.
async fn handle_mcp_post(
    State(server): State<SharedServer>,
    Json(request): Json<JsonRpcRawRequest>,
) -> Response {
    let mut srv = server.lock().await;

    match srv.handle_raw_message(request).await {
        Some(response) => {
            match serde_json::to_string(&response) {
                Ok(body) => Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
                Err(e) => {
                    tracing::error!("HTTP: failed to serialize response: {e}");
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header("Content-Type", "application/json")
                        .body(Body::from(
                            r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"Internal error"}}"#
                        ))
                        .unwrap()
                }
            }
        }
        None => {
            // Notification → 202 Accepted, no body
            Response::builder()
                .status(StatusCode::ACCEPTED)
                .body(Body::empty())
                .unwrap()
        }
    }
}

/// GET /health — Kubernetes-compatible liveness probe.
async fn handle_health() -> &'static str {
    "OK"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::serial_engine::new_shared_engine;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_engine() -> SharedEngine {
        let tmp = TempDir::new().unwrap();
        let mut values = HashMap::new();
        values.insert("DEV_HOST_IP".into(), "127.0.0.1".into());
        values.insert("SERIAL_PORT".into(), "59999".into());
        values.insert("RELAY_PORT".into(), "0".into());
        values.insert("RESET_CHANNEL".into(), "0".into());
        values.insert("MASKROM_CHANNEL".into(), "0".into());
        values.insert("HANG_TIMEOUT".into(), "60".into());
        values.insert("HANG_HYSTERESIS".into(), "3".into());
        values.insert("MAX_ARCHIVED_LOGS".into(), "10".into());
        values.insert("MAX_LOG_FILE_SIZE".into(), "100".into());
        values.insert("DUT_DIR".into(), ".dut-serial".into());
        values.insert("LOCK_DIR".into(), "/tmp/embedded-debug-test-locks".into());
        values.insert("LOGIN_USER".into(), "root".into());
        values.insert("LOGIN_PASS".into(), "".into());
        values.insert("UBOOT_INTERRUPT_STRATEGY".into(), "lava".into());
        let config = Config {
            values,
            config_path: None,
            project_dir: Some(tmp.path().to_path_buf()),
            format: crate::config::ConfigFormat::None,
        };
        new_shared_engine(config)
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        // Test the health handler directly (no server needed).
        let result = handle_health().await;
        assert_eq!(result, "OK");
    }

    #[tokio::test]
    async fn test_mcp_post_initialize() {
        let engine = create_test_engine();
        let server = Arc::new(Mutex::new(McpServer::new(engine)));

        let request = JsonRpcRawRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::Value::Number(1.into())),
            method: Some("initialize".to_string()),
            params: None,
        };

        let response = handle_mcp_post(State(server), Json(request)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_mcp_post_notification() {
        let engine = create_test_engine();
        let server = Arc::new(Mutex::new(McpServer::new(engine)));

        // First initialize (required before other methods).
        let init_req = JsonRpcRawRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::Value::Number(1.into())),
            method: Some("initialize".to_string()),
            params: None,
        };
        let _ = handle_mcp_post(State(server.clone()), Json(init_req)).await;

        // Then send a notification (no id → notification).
        let notif_req = JsonRpcRawRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some("notifications/initialized".to_string()),
            params: None,
        };
        let response = handle_mcp_post(State(server), Json(notif_req)).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
}
