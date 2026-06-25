//! MCP Server — 零框架 JSON-RPC 2.0 over stdio。
//!
//! Protocol: MCP (Model Context Protocol) 2024-11-05
//! Transport: stdio (newline-delimited JSON-RPC 2.0)

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::serial_engine::SharedEngine;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

// ── Tool definitions ──────────────────────────────────────────────────────────

struct ToolDef {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "serial_send_command",
            description: "Send a shell command to the target and return the output.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command to execute"},
                    "timeout": {"type": "integer", "default": 90, "description": "Timeout in seconds"},
                },
                "required": ["command"],
            }),
        },
        ToolDef {
            name: "serial_get_state",
            description: "Get the current target state and metadata.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_get_logs",
            description: "Retrieve serial log content with optional filtering.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "lines": {"type": "integer", "default": 50, "description": "Number of lines to return"},
                    "pattern": {"type": "string", "description": "Regex filter pattern"},
                    "archive": {"type": "integer", "default": 0, "description": "Archive index (0=current)"},
                },
            }),
        },
        ToolDef {
            name: "serial_list_logs",
            description: "List all archived boot logs.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_reset",
            description: "Hardware reset target via relay and rotate log.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "wait_boot": {"type": "boolean", "default": true, "description": "Wait for boot to complete"},
                    "failure_retry": {"type": "integer", "default": 3, "description": "Retry the reset+wait on timeout (lava RetryAction semantics)"},
                    "failure_retry_interval": {"type": "number", "default": 1.0, "description": "Seconds between retries"},
                },
            }),
        },
        ToolDef {
            name: "serial_enter_uboot",
            description: "Force target into U-Boot interactive prompt via relay reset + continuous Ctrl-C flood. Works even with bootdelay=0. Retries up to failure_retry times on timeout.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "failure_retry": {"type": "integer", "default": 3, "description": "Number of retry attempts on timeout"},
                    "failure_retry_interval": {"type": "number", "default": 1.0, "description": "Seconds between retries"},
                    "flood_duration_secs": {"type": "number", "default": 15.0, "description": "Total Ctrl-C flood duration (must cover SPL→U-Boot window, typically 3-8s)"},
                    "flood_interval_ms": {"type": "integer", "default": 100, "description": "Interval between Ctrl-C bytes (100ms = 10 bytes/s, avoids UART FIFO overflow)"},
                },
            }),
        },
        ToolDef {
            name: "serial_reboot_uboot",
            description: "Soft reboot from Linux + continuous Ctrl-C flood to enter U-Boot prompt. Works even with bootdelay=0. Retries up to failure_retry times on timeout.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "failure_retry": {"type": "integer", "default": 3, "description": "Number of retry attempts on timeout"},
                    "failure_retry_interval": {"type": "number", "default": 1.0, "description": "Seconds between retries"},
                    "flood_interval_ms": {"type": "integer", "default": 100, "description": "Interval between Ctrl-C bytes (100ms)"},
                },
            }),
        },
        ToolDef {
            name: "serial_enter_maskrom",
            description: "Force target into Rockchip MASKROM (loader) mode via relay sequence. Pulls MASKROM pin low, pulses RESET, then releases MASKROM. Target will appear as a Rockchip USB device on the Dev Host.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_wait_pattern",
            description: "Wait until a regex pattern appears in serial output.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex pattern to match"},
                    "timeout": {"type": "integer", "default": 60, "description": "Timeout in seconds"},
                    "action": {"type": "string", "description": "Optional action (e.g. send_ctrl_c)"},
                },
                "required": ["pattern"],
            }),
        },
        ToolDef {
            name: "serial_uboot_command",
            description: "Send a raw command at U-Boot prompt (=> ) and return output. Use after serial_enter_uboot.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "U-Boot command (e.g. 'version', 'help', 'reboot loader', 'rockusb 0 mmc 0')"},
                    "timeout": {"type": "integer", "default": 15, "description": "Timeout in seconds"},
                },
                "required": ["command"],
            }),
        },
        ToolDef {
            name: "serial_new_log",
            description: "Manually rotate log without hardware reset.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_poll_logs",
            description: "Get new serial output since last poll (long-polling).",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "since": {"type": "number", "description": "Timestamp from previous poll"},
                    "timeout": {"type": "integer", "default": 10, "description": "Long-poll timeout in seconds"},
                },
            }),
        },
        ToolDef {
            name: "serial_get_config",
            description: "Get current target configuration.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_get_metrics",
            description: "Get engine metrics: uptime, command count, error count, pending commands.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_claim",
            description: "Claim serial ownership for this session. Releases the lock from any other session and reconnects the serial. Only works if no other session is actively using the serial.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_load_reference",
            description: "Load a reference boot log to enable adaptive stage detection for a new/unknown SOC. The reference log should be a complete boot log (DDR→SPL→U-Boot→Kernel→Shell). After loading, the stage detector uses text similarity to identify stages instead of hardcoded regex patterns.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "reference_log_path": {
                        "type": "string",
                        "description": "Absolute path to the reference boot log file"
                    }
                },
                "required": ["reference_log_path"]
            }),
        },
        ToolDef {
            name: "serial_get_unclassified",
            description: "Get serial output lines that StageLearner could not classify into any known boot stage. Use this to identify new boot patterns, then call serial_append_reference to add them to the reference log for future detection.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_append_reference",
            description: "Append key anchor lines to the reference boot log and hot-reload StageLearner. Use after analyzing unclassified lines from serial_get_unclassified. The lines become new fingerprints — StageLearner will match them on future boot cycles without restart. Choose distinctive lines that mark a boot stage boundary (e.g. 'DDR fdeec6f4fc typ...', 'U-Boot SPL board init', 'Linux version 5.10.0'). Avoid lines with timestamps, memory addresses, or random numbers.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "string",
                        "description": "Key anchor lines to append (newline-separated). Pick distinctive lines that mark a boot stage boundary."
                    }
                },
                "required": ["lines"],
            }),
        },
        ToolDef {
            name: "serial_get_stages",
            description: "Get the learned stage fingerprints from the reference log (if loaded). Shows what patterns the adaptive detector uses for each boot stage.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_learn_connection",
            description: "Run connection learning to verify serial connectivity. Performs 3x hardware reset (if relay configured) or software reboot cycles, compares boot log similarity. If similarity >= 93%, generates reference boot log for stage detection. If relay reset similarity < 10%, marks relay as broken and suggests software reboot fallback.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "method": {
                        "type": "string",
                        "description": "Learning method: 'hardware' (relay reset, default), 'software' (reboot command), or 'auto' (try hardware first, fallback to software)"
                    },
                },
            }),
        },
        ToolDef {
            name: "serial_verify_relay",
            description: "Verify CH340 relay control by sending ON/OFF commands and reading back state. Returns whether the relay is controllable and responding correctly.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_button",
            description: "Control a DUT button (reset/recovery/maskrom) via the power control abstraction. Supports press, release, and pulse actions. Buttons must be configured in .target.toml [relay] section.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "button": {
                        "type": "string",
                        "description": "Button name: 'reset', 'maskrom', or 'recovery'"
                    },
                    "action": {
                        "type": "string",
                        "description": "Action: 'press', 'release', or 'pulse' (press+delay+release)"
                    },
                    "delay_ms": {
                        "type": "integer",
                        "description": "Delay in milliseconds for pulse action (default: 500)"
                    },
                },
                "required": ["button", "action"],
            }),
        },
        ToolDef {
            name: "serial_flash_plan",
            description: "Generate a flash plan from .target.toml [flash] config. Resolves symlinks, computes upload path, shows the flash commands that will be executed. Does NOT execute anything — use serial_flash to execute.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "image_path": {
                        "type": "string",
                        "description": "Local path to the firmware image (e.g. update.img or boot.img)"
                    },
                    "image_type": {
                        "type": "string",
                        "description": "Image type: 'full' (complete firmware) or 'kernel' (kernel/boot only). Default: auto-detect from flash config."
                    },
                },
                "required": ["image_path"],
            }),
        },
        ToolDef {
            name: "serial_pause",
            description: "Pause the serial engine — stops read loop, watchdog heartbeat, and all Agent-initiated serial output. Use before taking over the serial port with dutabo serial. Call serial_resume to resume.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_resume",
            description: "Resume the serial engine after serial_pause. Restarts read loop and watchdog.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "serial_send_raw",
            description: "Send raw bytes to the serial port without any markers or command wrapping. Use for interactive terminal sessions (dutabo serial).",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "data": {"type": "string", "description": "Raw data to send (supports \\n, \\r, \\x03 for Ctrl-C etc.)"},
                },
                "required": ["data"],
            }),
        },
        ToolDef {
            name: "serial_flash",
            description: "Execute firmware flash to target device. Uploads image to dev host, verifies sha256, optionally enters MASKROM and flashes loader, then flashes the main image. Requires [flash] section in .target.toml. WARNING: This modifies the target device firmware.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "image_path": {
                        "type": "string",
                        "description": "Local path to the firmware image (e.g. update.img)"
                    },
                    "image_type": {
                        "type": "string",
                        "description": "Image type: 'full' (default) or 'kernel'"
                    },
                    "skip_upload": {
                        "type": "boolean",
                        "description": "Skip upload if image already on dev host (default: false)"
                    },
                },
                "required": ["image_path"],
            }),
        },
    ]
}

// ── JSON-RPC types ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

/// Public raw request type for HTTP transport
#[derive(Deserialize, Debug)]
pub struct JsonRpcRawRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
}

impl From<JsonRpcRawRequest> for JsonRpcRequest {
    fn from(raw: JsonRpcRawRequest) -> Self {
        JsonRpcRequest {
            jsonrpc: raw.jsonrpc,
            id: raw.id,
            method: raw.method,
            params: raw.params,
        }
    }
}

#[derive(Serialize)]
pub struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
pub struct JsonRpcError {
    code: i32,
    message: String,
}

// ── MCP Server ────────────────────────────────────────────────────────────────

pub struct McpServer {
    tools: Vec<ToolDef>,
    pub initialized: bool,
    pub(crate) engine: SharedEngine,
}

impl McpServer {
    pub fn new(engine: SharedEngine) -> Self {
        Self {
            tools: tool_definitions(),
            initialized: false,
            engine,
        }
    }

    /// 主循环: 读 stdin → 处理 JSON-RPC → 写 stdout
    /// 串口 read loop 在独立 tokio task 中运行，避免 select! 饥饿
    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut writer = stdout;
        let mut line = String::new();

        // 事件驱动读循环 (epoll, 无轮询), 加防抖
        let engine_read = self.engine.clone();
        let read_handle = tokio::spawn(async move {
            loop {
                let mut eng = engine_read.lock().await;
                eng.read_loop_iter().await;
                drop(eng);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        // 独立 watchdog task
        let engine_wd = self.engine.clone();
        let watchdog_handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                let mut eng = engine_wd.lock().await;
                eng.watchdog_once();
            }
        });

        // 注册后台任务 handle 到 engine (确保 stop() 能正确清理)
        {
            let mut eng = self.engine.lock().await;
            eng.set_background_tasks(read_handle, watchdog_handle);
        }

        tracing::info!("[debug-console-mcp] stdio transport ready");

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<JsonRpcRequest>(trimmed) {
                        Ok(request) => {
                            if let Some(response) = self.handle_message(request).await {
                                let response_json = serde_json::to_string(&response)?;
                                writer.write_all(response_json.as_bytes()).await?;
                                writer.write_all(b"\n").await?;
                                writer.flush().await?;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Invalid JSON: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::info!("stdin error: {e}");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Public handler for HTTP transport (takes raw request, produces raw response)
    pub async fn handle_raw_message(
        &mut self,
        request: JsonRpcRawRequest,
    ) -> Option<JsonRpcResponse> {
        self.handle_message(request.into()).await
    }

    /// 处理 JSON-RPC 消息
    async fn handle_message(&mut self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        if request.jsonrpc != "2.0" {
            return Some(Self::error_response(
                request.id,
                -32600,
                "Invalid Request: jsonrpc must be '2.0'",
            ));
        }

        let method = match &request.method {
            Some(m) => m.clone(),
            None => {
                return Some(Self::error_response(
                    request.id,
                    -32600,
                    "Invalid Request: missing method",
                ));
            }
        };

        // notifications/initialized — no response
        if method == "notifications/initialized" {
            self.initialized = true;
            return None;
        }

        // 所有其他请求需要先 initialize
        if !self.initialized && method != "initialize" {
            return Some(Self::error_response(
                request.id,
                -32600,
                "Not initialized: send initialize first",
            ));
        }

        // 路由
        match method.as_str() {
            "initialize" => {
                let result = serde_json::json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": {"listChanged": false},
                        "resources": {"listChanged": false, "subscribe": false},
                        "prompts": {"listChanged": false},
                    },
                    "serverInfo": {
                        "name": "debug-console-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                });
                self.initialized = true;
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: Some(result),
                    error: None,
                })
            }
            "ping" => Some(JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: Some(serde_json::json!({})),
                error: None,
            }),
            "tools/list" => {
                let tools: Vec<Value> = self
                    .tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": t.input_schema,
                        })
                    })
                    .collect();
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: Some(serde_json::json!({"tools": tools})),
                    error: None,
                })
            }
            "tools/call" => {
                let params = request.params.unwrap_or(Value::Null);
                let result = self.handle_call_tool(params).await;
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: Some(result),
                    error: None,
                })
            }
            "resources/list" => {
                let result = {
                    let engine = self.engine.lock().await;
                    Self::build_resources_list(&engine)
                };
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: Some(result),
                    error: None,
                })
            }
            "resources/read" => {
                let uri = request
                    .params
                    .and_then(|p| p.get("uri").cloned())
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let result = {
                    let engine = self.engine.lock().await;
                    Self::build_resource_content(&engine, &uri)
                };
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: Some(serde_json::json!({
                        "contents": [result],
                    })),
                    error: None,
                })
            }
            "prompts/list" => {
                let result = Self::build_prompts();
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: Some(result),
                    error: None,
                })
            }
            "prompts/get" => {
                let name = request
                    .params
                    .and_then(|p| p.get("name").cloned())
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let result = Self::build_prompt_content(&name);
                Some(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: Some(result),
                    error: None,
                })
            }
            _ => Some(Self::error_response(
                request.id,
                -32601,
                &format!("Method not found: {method}"),
            )),
        }
    }

    /// 处理 tools/call 请求
    async fn handle_call_tool(&mut self, params: Value) -> Value {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);

        // serial_enter_uboot: relay reset + continuous Ctrl-C flood, then
        // await U-Boot prompt. Retries up to `failure_retry` times.
        //
        // CRITICAL: The flood must release the engine lock between bursts so
        // the read loop can process U-Boot banners and trigger the watcher.
        // The old code held the lock for the entire flood (1.6s), blocking
        // the read loop — the watcher never fired even when U-Boot appeared.
        //
        // For bootdelay=0: SPL/BL31/OP-TEE don't read the serial port, so
        // Ctrl-C chars accumulate in the UART FIFO. When U-Boot's abortboot()
        // calls tstc(), even ONE pending Ctrl-C interrupts. We send 1 byte
        // per 100ms for up to 15s, releasing the lock between each burst.
        if name == "serial_enter_uboot" {
            let failure_retry = args
                .get("failure_retry")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            let failure_retry_interval = args
                .get("failure_retry_interval")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);
            let flood_duration_secs = args
                .get("flood_duration_secs")
                .and_then(|v| v.as_f64())
                .unwrap_or(15.0);
            let flood_interval_ms = args
                .get("flood_interval_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(100);

            let mut last_error = String::new();
            for attempt in 1..=failure_retry {
                // Phase 1: relay reset + initial burst (holds lock ~1s).
                let mut rx = {
                    let mut engine = self.engine.lock().await;
                    let rx = engine.queue_enter_uboot();
                    if let Err(e) = engine.do_relay_reset_and_flood().await {
                        return serde_json::json!({
                            "content": [{"type": "text", "text": serde_json::to_string(&serde_json::json!({"success": false, "error": e})).unwrap_or_default()}]
                        });
                    }
                    rx
                };

                // Phase 2: continuous low-rate Ctrl-C flood, RELEASING the
                // lock between each byte so the read loop can process
                // U-Boot banners and trigger the watcher.
                let flood_rounds = (flood_duration_secs * 1000.0 / flood_interval_ms as f64) as u64;
                let mut matched: Option<Result<crate::boot_detector::WatcherMatch, ()>> = None;
                for i in 0..flood_rounds {
                    // Check if watcher already fired (non-blocking).
                    match rx.try_recv() {
                        Ok(m) => {
                            matched = Some(Ok(m));
                            break;
                        }
                        Err(_) => {} // Empty or closed — keep flooding
                    }
                    // Send one Ctrl-C, then release lock for the interval.
                    {
                        let mut engine = self.engine.lock().await;
                        engine.flood_one().await;
                    }
                    // Release lock and let read loop process data.
                    tokio::time::sleep(std::time::Duration::from_millis(flood_interval_ms)).await;
                    if i % 50 == 0 {
                        tracing::debug!("enter_uboot flood: {}/{} rounds", i, flood_rounds);
                    }
                }

                // Phase 3: if watcher hasn't fired yet, wait a bit more
                // (U-Boot may have appeared in the last interval).
                if matched.is_none() {
                    match tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await {
                        Ok(Some(m)) => matched = Some(Ok(m)),
                        _ => matched = Some(Err(())),
                    }
                }

                let result = {
                    let mut engine = self.engine.lock().await;
                    let pattern = r"=>|U-Boot[>#]";
                    engine.detector.remove_watcher_by_pattern(pattern);
                    match matched {
                        Some(Ok(_m)) => {
                            engine
                                .state
                                .transition(crate::state_manager::TargetState::UBoot);
                            serde_json::json!({"success": true, "state_after": "uboot", "attempts": attempt})
                        }
                        _ => {
                            last_error = "Timed out waiting for U-Boot prompt".to_string();
                            serde_json::json!({
                                "success": false,
                                "state_after": engine.state.current().as_str(),
                                "error": &last_error,
                                "attempts": attempt,
                            })
                        }
                    }
                };
                if result["success"].as_bool().unwrap_or(false) {
                    return serde_json::json!({
                        "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
                    });
                }
                if attempt < failure_retry {
                    tokio::time::sleep(std::time::Duration::from_secs_f64(failure_retry_interval))
                        .await;
                }
            }
            let result = serde_json::json!({
                "success": false,
                "state_after": self.engine.lock().await.state.current().as_str(),
                "error": format!("{last_error} after {failure_retry} attempts"),
                "attempts": failure_retry,
            });
            return serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
            });
        }

        // serial_reboot_uboot: soft reboot + continuous Ctrl-C flood to
        // enter U-Boot. Retries up to `failure_retry` times.
        //
        // Same flood strategy as serial_enter_uboot: release lock between
        // Ctrl-C bursts so read loop can process banners.
        if name == "serial_reboot_uboot" {
            let failure_retry = args
                .get("failure_retry")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;
            let failure_retry_interval = args
                .get("failure_retry_interval")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);
            let flood_interval_ms = args
                .get("flood_interval_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(100);

            let mut last_error = String::new();
            for attempt in 1..=failure_retry {
                // Phase 1: send reboot + set up SPL watcher, release lock.
                let mut spl_rx = {
                    let mut engine = self.engine.lock().await;
                    engine
                        .state
                        .transition(crate::state_manager::TargetState::Booting);
                    engine.console.sendline("reboot");
                    engine.console.drain_writes().await;
                    engine.logs.flush_boot_log();
                    engine.logs.mark_boot_start();
                    engine.detector.reset_cycle();
                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                    engine.detector.add_watcher(r"U-Boot\s+SPL", tx);
                    rx
                };
                // Phase 2: wait for SPL without lock.
                let _spl_ok =
                    tokio::time::timeout(std::time::Duration::from_secs(30), spl_rx.recv()).await;
                // Phase 3: SPL detected → arm U-Boot watcher + start flood.
                let mut uboot_rx = {
                    let mut engine = self.engine.lock().await;
                    engine.detector.remove_watcher_by_pattern(r"U-Boot\s+SPL");
                    engine.queue_enter_uboot()
                };

                // Phase 4: continuous low-rate Ctrl-C flood, RELEASING the
                // lock between bursts so read loop can process U-Boot banner
                // and trigger the watcher. 1 byte per 100ms for 15 seconds.
                let flood_rounds = 15000u64 / flood_interval_ms;
                let mut matched: Option<Result<crate::boot_detector::WatcherMatch, ()>> = None;
                for i in 0..flood_rounds {
                    match uboot_rx.try_recv() {
                        Ok(m) => {
                            matched = Some(Ok(m));
                            break;
                        }
                        Err(_) => {}
                    }
                    {
                        let mut engine = self.engine.lock().await;
                        engine.flood_one().await;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(flood_interval_ms)).await;
                    if i % 50 == 0 {
                        tracing::debug!("reboot_uboot flood: {}/{} rounds", i, flood_rounds);
                    }
                }

                if matched.is_none() {
                    match tokio::time::timeout(std::time::Duration::from_secs(3), uboot_rx.recv())
                        .await
                    {
                        Ok(Some(m)) => matched = Some(Ok(m)),
                        _ => matched = Some(Err(())),
                    }
                }

                let result = {
                    let mut engine = self.engine.lock().await;
                    let pattern = r"=>|U-Boot[>#]";
                    engine.detector.remove_watcher_by_pattern(pattern);
                    match matched {
                        Some(Ok(_m)) => {
                            engine
                                .state
                                .transition(crate::state_manager::TargetState::UBoot);
                            serde_json::json!({"success": true, "state_after": "uboot", "attempts": attempt})
                        }
                        _ => {
                            last_error = "Timed out waiting for U-Boot prompt".to_string();
                            serde_json::json!({
                                "success": false,
                                "state_after": engine.state.current().as_str(),
                                "error": &last_error,
                                "attempts": attempt,
                            })
                        }
                    }
                };
                if result["success"].as_bool().unwrap_or(false) {
                    return serde_json::json!({
                        "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
                    });
                }
                if attempt < failure_retry {
                    tokio::time::sleep(std::time::Duration::from_secs_f64(failure_retry_interval))
                        .await;
                }
            }
            let result = serde_json::json!({
                "success": false,
                "state_after": self.engine.lock().await.state.current().as_str(),
                "error": format!("{last_error} after {failure_retry} attempts"),
                "attempts": failure_retry,
            });
            return serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
            });
        }

        // serial_wait_pattern: release lock, await watcher, avoid read-loop
        // deadlock. Supports `probe_on_timeout` (lava `force_prompt_wait`):
        // on timeout, send a newline to provoke a prompt and retry up to 6
        // times at `timeout/10` each.
        if name == "serial_wait_pattern" {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let timeout = args.get("timeout").and_then(|v| v.as_f64()).unwrap_or(60.0);
            let send_ctrl_c = args
                .get("action")
                .and_then(|v| v.as_str())
                .is_some_and(|a| a == "send_ctrl_c");
            // Auto-enable probe for prompt-like patterns (cheap heuristic).
            let probe_on_timeout = pattern.contains("login")
                || pattern.contains("prompt")
                || pattern.contains("=>")
                || pattern.contains(r"\$")
                || pattern.contains("#");

            // Phase 1: acquire lock briefly to set up the watcher, then
            // release. The read loop feeds watchers — no lock needed to await.
            let console_tx = {
                let engine = self.engine.lock().await;
                engine.console.write_sender()
            };
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            {
                let mut engine = self.engine.lock().await;
                engine
                    .detector
                    .add_watcher(&pattern, tx.clone());
            }

            // Phase 2: await outside the lock — read loop can still process
            // serial data and feed watchers.
            let result = crate::serial_engine::wait_pattern_with_probe(
                &mut rx,
                timeout,
                probe_on_timeout,
                &console_tx,
            )
            .await;

            // Phase 3: cleanup — remove the watcher (brief lock).
            {
                let mut engine = self.engine.lock().await;
                engine.detector.remove_watcher_by_pattern(&pattern);
            }

            let result = if send_ctrl_c && result["matched"].as_bool().unwrap_or(false) {
                let engine = self.engine.lock().await;
                engine.console.sendcontrol('c');
                serde_json::json!({"matched": true, "matched_line": null})
            } else {
                result
            };
            return serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
            });
        }

        // serial_send_command 需要在 lock 外 await，避免与 read_once 死锁
        if name == "serial_send_command" {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let timeout = args.get("timeout").and_then(|v| v.as_f64()).unwrap_or(90.0);
            let span = tracing::info_span!(
                "serial_send_command",
                cmd = %command,
                timeout,
                result.output = tracing::field::Empty,
                result.exit_code = tracing::field::Empty,
                result.timed_out = tracing::field::Empty,
            );
            let _guard = span.enter();
            let cmd_trimmed = command.trim();
            if cmd_trimmed == "reboot" || cmd_trimmed == "poweroff" || cmd_trimmed == "shutdown" {
                let mut engine = self.engine.lock().await;
                engine.console.sendline(command);
                engine.console.drain_writes().await;
                span.record("result.output", format!("{cmd_trimmed} sent"));
                span.record("result.exit_code", 0);
                span.record("result.timed_out", false);
                return serde_json::json!({
                    "content": [{"type": "text", "text": serde_json::to_string(&serde_json::json!({"output": format!("{cmd_trimmed} sent"), "exit_code": 0, "timed_out": false})).unwrap_or_default()}]
                });
            }
            let rx = {
                let mut engine = self.engine.lock().await;
                engine.queue_command(command, timeout)
            };
            let result = match tokio::time::timeout(
                std::time::Duration::from_secs_f64(timeout),
                rx
            ).await {
                Ok(Ok(r)) => {
                    let mut res = serde_json::json!({"output": r.output, "exit_code": r.exit_code, "timed_out": r.timed_out, "truncated": r.truncated});
                    span.record("result.output", &r.output);
                    span.record("result.exit_code", r.exit_code);
                    span.record("result.timed_out", r.timed_out);
                    if r.output.is_empty() && r.exit_code.is_none() && command.contains('|') {
                        res["hint"] = serde_json::json!(
                            "Empty output with pipe detected (BusyBox ash buffering). Retry: use printf instead of echo, or append '; true' after the pipe. See skill SKILL.md § Known Limitations."
                        );
                    }
                    if r.timed_out {
                        let eng = self.engine.lock().await;
                        let host = eng.config.dev_host_ip();
                        let port = eng.config.serial_target();
                        res["hint"] = serde_json::json!(
                            format!("Command timed out after {timeout:.0}s on {host}:{port} — target may be booting or unresponsive. Check serial_get_state.")
                        );
                    }
                    res
                }
                Ok(Err(_)) => serde_json::json!({"error": "Command cancelled"}),
                Err(_elapsed) => {
                    let eng = self.engine.lock().await;
                    let host = eng.config.dev_host_ip();
                    let port = eng.config.serial_target();
                    let mut res = serde_json::json!({
                        "output": "(timeout — engine did not respond)",
                        "exit_code": null,
                        "timed_out": true
                    });
                    res["hint"] = serde_json::json!(
                        format!("Engine timeout after {timeout:.0}s on {host}:{port} — MCP may be stuck. Try serial_get_state to check, or restart MCP.")
                    );
                    res
                }
            };
            return serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
            });
        }

        let result = {
            let mut engine = self.engine.lock().await;
            self.call_tool_impl(&mut engine, name, &args).await
        };

        serde_json::json!({
            "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
        })
    }

    /// Tool 实现分发
    async fn call_tool_impl(
        &self,
        engine: &mut crate::serial_engine::SerialEngine,
        name: &str,
        args: &Value,
    ) -> Value {
        // Note: serial_send_command and serial_enter_uboot are intercepted in
        // handle_call_tool to release the engine lock during network I/O.
        match name {
            "serial_get_state" => engine.get_state_dict(),
            "serial_get_logs" => {
                let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let pattern = args.get("pattern").and_then(|v| v.as_str());
                let archive = args.get("archive").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                engine.read_log(archive, lines, pattern)
            }
            "serial_list_logs" => engine.list_logs(),
            "serial_reset" => {
                let wait_boot = args
                    .get("wait_boot")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let failure_retry = args
                    .get("failure_retry")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3) as usize;
                let failure_retry_interval = args
                    .get("failure_retry_interval")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0);
                engine
                    .reset_target(wait_boot, failure_retry, failure_retry_interval)
                    .await
            }
            "serial_enter_maskrom" => engine.enter_maskrom().await,
            // serial_wait_pattern moved to handle_call_tool (lock release)
            "serial_uboot_command" => {
                let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let timeout = args.get("timeout").and_then(|v| v.as_f64()).unwrap_or(15.0);
                engine.send_uboot_command(command, timeout).await
            }
            "serial_new_log" => engine.rotate_log(),
            "serial_poll_logs" => engine.poll_logs(),
            "serial_get_config" => engine.get_config(),
            "serial_get_metrics" => serde_json::json!({
                "uptime_secs": engine.state.uptime_secs(),
                "command_count": engine.state.command_count(),
                "error_count": engine.state.error_count(),
                "pending_commands": engine.commands.pending_len(),
                "completed": engine.commands.completed_count,
                "cmd_errors": engine.commands.error_count,
            }),
            "serial_claim" => engine.claim_serial().await,
            "serial_load_reference" => {
                let path_str = args
                    .get("reference_log_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if path_str.is_empty() {
                    serde_json::json!({"success": false, "error": "reference_log_path required"})
                } else {
                    let path = std::path::PathBuf::from(path_str);
                    match engine.detector.load_reference(&path) {
                        Ok(()) => serde_json::json!({
                            "success": true,
                            "message": format!("Reference loaded from {}", path_str),
                            "fingerprints": engine.detector.learner.as_ref().map(|l| l.fingerprints.len()).unwrap_or(0),
                        }),
                        Err(e) => serde_json::json!({"success": false, "error": e}),
                    }
                }
            }
            "serial_get_unclassified" => engine.get_unclassified(),
            "serial_append_reference" => {
                let lines = args.get("lines").and_then(|v| v.as_str()).unwrap_or("");
                if lines.is_empty() {
                    serde_json::json!({"success": false, "error": "lines parameter required"})
                } else {
                    engine.append_reference(lines)
                }
            }
            "serial_get_stages" => {
                match &engine.detector.learner {
                    Some(learner) => {
                        let fps: Vec<serde_json::Value> = learner.export_fingerprints()
                            .into_iter()
                            .map(|(stage, anchor)| serde_json::json!({"stage": stage, "anchor": anchor}))
                            .collect();
                        serde_json::json!({"fingerprints": fps, "count": fps.len()})
                    }
                    None => {
                        serde_json::json!({"fingerprints": [], "count": 0, "message": "No reference loaded. Use serial_load_reference first."})
                    }
                }
            }
            "serial_learn_connection" => {
                let method = args
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("hardware");
                match method {
                    "software" | "reboot" => engine.learn_connection_software().await,
                    "auto" => {
                        let result = engine.learn_connection_hardware().await;
                        if result["success"].as_bool().unwrap_or(false) {
                            result
                        } else {
                            let sw_result = engine.learn_connection_software().await;
                            serde_json::json!({
                                "hardware_result": result,
                                "software_result": sw_result,
                                "success": sw_result["success"],
                                "method_used": "software_reboot",
                            })
                        }
                    }
                    _ => engine.learn_connection_hardware().await,
                }
            }
            "serial_verify_relay" => engine.verify_relay().await,
            "serial_pause" => engine.pause(),
            "serial_resume" => engine.resume(),
            "serial_send_raw" => {
                let data = args.get("data").and_then(|v| v.as_str()).unwrap_or("");
                engine.send_raw(data)
            }
            "serial_button" => {
                let button = args.get("button").and_then(|v| v.as_str()).unwrap_or("");
                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let delay_ms = args.get("delay_ms").and_then(|v| v.as_u64());
                engine.control_button(button, action, delay_ms).await
            }
            "serial_flash_plan" => {
                let image_path = args
                    .get("image_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let image_type = args
                    .get("image_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("full");
                if image_path.is_empty() {
                    serde_json::json!({"success": false, "error": "image_path required"})
                } else {
                    Self::build_flash_plan(&engine.config, image_path, image_type)
                }
            }
            "serial_flash" => {
                let image_path = args
                    .get("image_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if image_path.is_empty() {
                    serde_json::json!({"success": false, "error": "image_path required"})
                } else {
                    // Flash execution requires releasing lock — done inline for now
                    serde_json::json!({
                        "success": false,
                        "error": "Flash execution requires dev host SSH access. Use dutabo CLI: dutabo uf <image>",
                        "hint": "The MCP server connects to serial, not SSH. Flash is executed via CLI.",
                        "flash_plan": Self::build_flash_plan(&engine.config, image_path, "full"),
                    })
                }
            }
            _ => serde_json::json!({"error": format!("Unknown tool: {name}")}),
        }
    }

    // ── Resources ──

    fn build_resources_list(engine: &crate::serial_engine::SerialEngine) -> Value {
        let archives = engine.logs.list_archives();
        let mut resources = Vec::new();

        for a in &archives {
            let uri = format!("log://boot/{}", a.index);
            resources.push(serde_json::json!({
                "uri": uri,
                "name": format!("Boot {} — {}", a.index, a.filename),
                "description": format!("Boot log archive #{} ({} bytes)", a.index, a.size_bytes),
                "mimeType": "text/plain",
            }));
        }

        // Also expose current state as a resource
        resources.push(serde_json::json!({
            "uri": "state://target",
            "name": "Target State",
            "description": "Current DUT state and metadata",
            "mimeType": "application/json",
        }));

        serde_json::json!({"resources": resources})
    }

    fn build_resource_content(engine: &crate::serial_engine::SerialEngine, uri: &str) -> Value {
        if uri == "state://target" {
            let state = engine.get_state_dict();
            return serde_json::json!({
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&state).unwrap_or_default(),
            });
        }

        // log://boot/{index}
        if let Some(index_str) = uri.strip_prefix("log://boot/") {
            if let Ok(index) = index_str.parse::<usize>() {
                let result = engine.logs.read_log(index, 500, None);
                return serde_json::json!({
                    "uri": uri,
                    "mimeType": "text/plain",
                    "text": result.content,
                });
            }
        }

        serde_json::json!({
            "uri": uri,
            "mimeType": "text/plain",
            "text": format!("Resource not found: {uri}"),
        })
    }

    // ── Prompts ──

    fn build_prompts() -> Value {
        serde_json::json!({
            "prompts": [
                {
                    "name": "debug_boot",
                    "description": "Debug target boot process — monitor SPL, U-Boot, kernel, login",
                    "arguments": []
                },
                {
                    "name": "debug_kernel",
                    "description": "Diagnose kernel crashes from serial logs",
                    "arguments": [
                        {
                            "name": "lines",
                            "description": "Number of recent log lines to analyze",
                            "required": false
                        }
                    ]
                },
                {
                    "name": "check_status",
                    "description": "Quick target health check — state + recent output",
                    "arguments": []
                },
                {
                    "name": "send_and_check",
                    "description": "Send a command to the target and verify the output",
                    "arguments": [
                        {
                            "name": "command",
                            "description": "Shell command to execute on target",
                            "required": true
                        }
                    ]
                },
                {
                    "name": "boot-capture",
                    "description": "Capture a clean boot log for StageLearner. Resets the target and waits for login.",
                    "arguments": []
                },
                {
                    "name": "crash-diagnose",
                    "description": "Check if target has crashed and retrieve the crash log.",
                    "arguments": []
                },
                {
                    "name": "uboot-recovery",
                    "description": "Enter U-Boot and recover from a bad kernel by flashing a new boot image.",
                    "arguments": []
                }
            ]
        })
    }

    fn build_prompt_content(name: &str) -> Value {
        match name {
            "debug_boot" => serde_json::json!({
                "description": "Debug target boot process",
                "messages": [{
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": "Monitor the target boot process. Use serial_reset to reset the target, wait for each boot stage using serial_wait_pattern, and report the boot progress. Check for any errors or warnings in the boot log.",
                    }
                }]
            }),
            "debug_kernel" => serde_json::json!({
                "description": "Diagnose kernel crashes",
                "messages": [{
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": "Diagnose kernel crashes from the serial logs. Use serial_get_logs with pattern='panic|BUG|Oops|Call trace' to find crash information. Analyze the crash dump and suggest the likely root cause.",
                    }
                }]
            }),
            "check_status" => serde_json::json!({
                "description": "Quick target health check",
                "messages": [{
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": "Check the target status. Use serial_get_state to get the current status, serial_get_logs with lines=20 to get recent output, and report the target's health.",
                    }
                }]
            }),
            "send_and_check" => serde_json::json!({
                "description": "Send command to target",
                "messages": [{
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": "Send a command to the target via serial_send_command and verify the output. Report the command result.",
                    }
                }]
            }),
            "boot-capture" => serde_json::json!({
                "description": "Capture a clean boot log for StageLearner",
                "messages": [{
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": "Run serial_reset(wait_boot=true) to capture a clean boot cycle. Then run serial_get_logs(lines=200) to review the boot log. Check serial_get_state to confirm the target reached active state.",
                    }
                }]
            }),
            "crash-diagnose" => serde_json::json!({
                "description": "Check if target has crashed and retrieve crash log",
                "messages": [{
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": "Run serial_get_state — if state is 'crashed', run serial_get_logs(lines=200, pattern='panic|BUG|Oops|Call trace') to see the crash details. Then consider rebooting or entering U-Boot for recovery.",
                    }
                }]
            }),
            "uboot-recovery" => serde_json::json!({
                "description": "Enter U-Boot for recovery",
                "messages": [{
                    "role": "user",
                    "content": {
                        "type": "text",
                        "text": "Run serial_enter_uboot to get to the U-Boot prompt. From there, use serial_uboot_command('setenv bootdelay 3') and serial_uboot_command('saveenv') to make U-Boot interactive. Then use serial_uboot_command('boot') to continue booting.",
                    }
                }]
            }),
            _ => serde_json::json!({
                "description": format!("Unknown prompt: {name}"),
                "messages": []
            }),
        }
    }

    /// Build a flash plan from config + local image path.
    fn build_flash_plan(
        config: &crate::config::Config,
        image_path: &str,
        image_type: &str,
    ) -> serde_json::Value {
        use crate::flash::{FlashConfig, ImageType};
        let flash_cfg = FlashConfig::from_config(&config.values);

        if !flash_cfg.is_configured() {
            return serde_json::json!({
                "success": false,
                "error": "Flash not configured. Add [flash] section to .target.toml.",
            });
        }

        let local_path = std::path::PathBuf::from(image_path);
        let real_path = FlashConfig::resolve_symlink(&local_path);
        let fname = real_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("firmware.img");

        let upload_dir = if flash_cfg.upload_dir.is_empty() {
            "/tmp".to_string()
        } else {
            flash_cfg.upload_dir.clone()
        };
        let remote_path = format!("{upload_dir}/{fname}");

        let dev_host = config.dev_host_ip();
        let dev_user = config.get_str_or("DEV_HOST_USER", "linaro");
        let parsed_image_type = ImageType::from_str(image_type).unwrap_or(ImageType::Full);
        let selected_flash_cmd = match parsed_image_type {
            ImageType::Full => flash_cfg.full_image_command(&remote_path),
            ImageType::Kernel => flash_cfg.kernel_image_command(&remote_path),
        };
        let loader_cmd = if flash_cfg.loader_bin.is_empty() {
            String::new()
        } else {
            flash_cfg.loader_command()
        };
        let list_devices_cmd = flash_cfg.list_devices_command();

        serde_json::json!({
            "success": true,
            "tool": flash_cfg.tool,
            "local_image": real_path.to_string_lossy(),
            "remote_path": remote_path,
            "dev_host": dev_host,
            "dev_user": dev_user,
            "upload_dir": upload_dir,
            "image_type": match parsed_image_type {
                ImageType::Full => "full",
                ImageType::Kernel => "kernel",
            },
            "full_image_cmd": flash_cfg.full_image_command(&remote_path),
            "kernel_image_cmd": flash_cfg.kernel_image_command(&remote_path),
            "selected_flash_cmd": selected_flash_cmd,
            "loader_bin": flash_cfg.loader_bin,
            "loader_cmd": loader_cmd,
            "list_devices_cmd": list_devices_cmd,
            "steps": [
                format!("scp {image_path} {dev_user}@{dev_host}:{remote_path}"),
                format!("ssh {dev_user}@{dev_host} 'sha256sum {remote_path}'"),
                format!("ssh {dev_user}@{dev_host} '{tool} {cmd}'", tool = flash_cfg.tool, cmd = selected_flash_cmd),
            ],
        })
    }

    fn error_response(id: Option<Value>, code: i32, message: &str) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
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
        values.insert("LOCK_DIR".into(), "/tmp/debug-console-test-locks".into());
        values.insert("LOGIN_USER".into(), "root".into());
        values.insert("LOGIN_PASS".into(), "".into());
        values.insert("UBOOT_INTERRUPT_STRATEGY".into(), "lava".into());

        let config = Config {
            values,
            config_path: None,
            project_dir: Some(tmp.path().to_path_buf()),
            format: crate::config::ConfigFormat::None,
        };

        crate::serial_engine::new_shared_engine(config)
    }

    fn make_request(id: i64, method: &str, params: Option<Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(id.into())),
            method: Some(method.to_string()),
            params,
        }
    }

    #[tokio::test]
    async fn test_initialize() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);

        let req = make_request(1, "initialize", None);
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.result.is_some());
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "debug-console-mcp");
    }

    #[tokio::test]
    async fn test_ping() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);

        // Initialize first
        server.initialized = true;

        let req = make_request(1, "ping", None);
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn test_tools_list() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);
        server.initialized = true;

        let req = make_request(1, "tools/list", None);
        let resp = server.handle_message(req).await.unwrap();

        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 27, "Expected 27 MCP tools");

        // Check some tool names
        let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(tool_names.contains(&"serial_send_command"));
        assert!(tool_names.contains(&"serial_get_state"));
        assert!(tool_names.contains(&"serial_get_logs"));
    }

    #[tokio::test]
    async fn test_not_initialized() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);

        // Try to call tools/list without initializing
        let req = make_request(1, "tools/list", None);
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.error.is_some());
        let error = resp.error.unwrap();
        assert_eq!(error.code, -32600);
        assert!(error.message.contains("Not initialized"));
    }

    #[tokio::test]
    async fn test_invalid_jsonrpc_version() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);

        let req = JsonRpcRequest {
            jsonrpc: "1.0".to_string(),
            id: Some(Value::Number(1.into())),
            method: Some("initialize".to_string()),
            params: None,
        };
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.error.is_some());
        let error = resp.error.unwrap();
        assert_eq!(error.code, -32600);
        assert!(error.message.contains("jsonrpc must be '2.0'"));
    }

    #[tokio::test]
    async fn test_missing_method() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::Number(1.into())),
            method: None,
            params: None,
        };
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.error.is_some());
        let error = resp.error.unwrap();
        assert_eq!(error.code, -32600);
        assert!(error.message.contains("missing method"));
    }

    #[tokio::test]
    async fn test_unknown_method() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);
        server.initialized = true;

        let req = make_request(1, "unknown/method", None);
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.error.is_some());
        let error = resp.error.unwrap();
        assert_eq!(error.code, -32601);
    }

    #[tokio::test]
    async fn test_notifications_initialized() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some("notifications/initialized".to_string()),
            params: None,
        };
        let resp = server.handle_message(req).await;

        // notifications should return None (no response)
        assert!(resp.is_none());
        assert!(server.initialized);
    }

    #[tokio::test]
    async fn test_tools_call_get_state() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);
        server.initialized = true;

        let params = serde_json::json!({
            "name": "serial_get_state",
            "arguments": {}
        });
        let req = make_request(1, "tools/call", Some(params));
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.result.is_some());
        let result = resp.result.unwrap();
        assert!(result["content"].is_array());
    }

    #[tokio::test]
    async fn test_tools_call_unknown_tool() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);
        server.initialized = true;

        let params = serde_json::json!({
            "name": "unknown_tool",
            "arguments": {}
        });
        let req = make_request(1, "tools/call", Some(params));
        let resp = server.handle_message(req).await.unwrap();

        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Unknown tool"));
    }

    #[tokio::test]
    async fn test_tools_call_list_logs() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);
        server.initialized = true;

        let params = serde_json::json!({
            "name": "serial_list_logs",
            "arguments": {}
        });
        let req = make_request(1, "tools/call", Some(params));
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.result.is_some());
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("archives"));
    }

    #[tokio::test]
    async fn test_tools_call_get_config() {
        let engine = create_test_engine();
        let mut server = McpServer::new(engine);
        server.initialized = true;

        let params = serde_json::json!({
            "name": "serial_get_config",
            "arguments": {}
        });
        let req = make_request(1, "tools/call", Some(params));
        let resp = server.handle_message(req).await.unwrap();

        assert!(resp.result.is_some());
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("DEV_HOST_IP"));
    }

    #[tokio::test]
    async fn test_error_response_format() {
        let resp = McpServer::error_response(Some(Value::Number(1.into())), -32600, "test error");

        assert_eq!(resp.jsonrpc, "2.0");
        assert_eq!(resp.id, Some(Value::Number(1.into())));
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());

        let error = resp.error.unwrap();
        assert_eq!(error.code, -32600);
        assert_eq!(error.message, "test error");
    }
}
