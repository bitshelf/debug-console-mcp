//! Streamable HTTP transport — MCP 2025-03-26 spec
//!
//! POST /mcp  → JSON-RPC 2.0 request → JSON response
//! GET /health → liveness probe
//!
//! SSE is deprecated per MCP 2025 spec; Streamable HTTP uses a single
//! endpoint for both requests and (optionally) server→client notifications.

use std::sync::Arc;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::sync::Mutex;

use crate::mcp::{JsonRpcRawRequest, McpServer};
use crate::serial_engine::SharedEngine;

type SharedServer = Arc<Mutex<McpServer>>;

/// Run the Streamable HTTP server on the given host:port
pub async fn run_http(
    engine: SharedEngine,
    bind_host: &str,
    bind_port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    // Spawn read loop: 事件驱动 (epoll, 无轮询), 加防抖防止 CPU 占用
    let engine_read = engine.clone();
    let read_handle = tokio::spawn(async move {
        loop {
            let mut eng = engine_read.lock().await;
            eng.read_loop_iter().await;
            drop(eng);
            // 每次迭代后短暂休眠, 避免数据洪水造成 busy loop
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });

    // Spawn watchdog: 每 2s 检查挂死/心跳
    let engine_wd = engine.clone();
    let watchdog_handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            let mut eng = engine_wd.lock().await;
            eng.watchdog_once();
        }
    });

    // 注册后台任务 handle 到 engine (确保 stop() 能正确清理)
    {
        let mut eng = engine.lock().await;
        eng.set_background_tasks(read_handle, watchdog_handle);
    }

    let server = Arc::new(Mutex::new(McpServer::new(engine)));

    let app = Router::new()
        .route("/mcp", post(handle_mcp_post))
        .route("/health", get(handle_health))
        .with_state(server);

    let addr = format!("{bind_host}:{bind_port}");
    tracing::info!("[embedded-debug-mcp] Streamable HTTP listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// POST /mcp — Main JSON-RPC endpoint
async fn handle_mcp_post(
    State(server): State<SharedServer>,
    Json(request): Json<JsonRpcRawRequest>,
) -> Response {
    let mut srv = server.lock().await;

    match srv.handle_raw_message(request).await {
        Some(response) => {
            let body = serde_json::to_string(&response).unwrap_or_default();
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(body))
                .unwrap()
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

/// GET /health — Kubernetes-compatible liveness probe
async fn handle_health() -> &'static str {
    "OK"
}
