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
                },
            }),
        },
        ToolDef {
            name: "serial_enter_uboot",
            description: "Force target into U-Boot interactive prompt via relay reset + Ctrl-C flood.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
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
                    "command": {"type": "string", "description": "U-Boot command (e.g. 'version', 'help', 'reboot loader')"},
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
            name: "serial_get_stages",
            description: "Get the learned stage fingerprints from the reference log (if loaded). Shows what patterns the adaptive detector uses for each boot stage.",
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
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
pub(crate) struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

// ── MCP Server ────────────────────────────────────────────────────────────────

pub struct McpServer {
    tools: Vec<ToolDef>,
    initialized: bool,
    engine: SharedEngine,
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

        tracing::info!("[embedded-debug-mcp] stdio transport ready");

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
    pub async fn handle_raw_message(&mut self, request: JsonRpcRawRequest) -> Option<JsonRpcResponse> {
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
                        "name": "embedded-debug-mcp",
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
                let uri = request.params
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
                let name = request.params
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
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Null);

        // serial_enter_uboot: 释放锁后 await prompt
        if name == "serial_enter_uboot" {
            let mut rx = {
                let mut engine = self.engine.lock().await;
                if !engine.relay.configured() {
                    return serde_json::json!({
                        "content": [{"type": "text", "text": "{\"success\": false, \"error\": \"No relay configured\"}"}]
                    });
                }
                let rx = engine.queue_enter_uboot();
                engine.do_relay_reset_and_flood().await;
                rx
            };
            let matched = tokio::time::timeout(std::time::Duration::from_secs(20), rx.recv()).await;
            let result = {
                let mut engine = self.engine.lock().await;
                // BUGFIX: always cleanup watcher to prevent memory leak
                let pattern = r"=>|U-Boot[>#]";
                engine.detector.remove_watcher_by_pattern(pattern);
                if let Ok(Some(_line)) = matched {
                    engine.state.transition(crate::state_manager::TargetState::UBoot);
                    serde_json::json!({"success": true, "state_after": "uboot"})
                } else {
                    serde_json::json!({"success": false, "state_after": engine.state.current().as_str(), "error": "Timed out waiting for U-Boot prompt"})
                }
            };
            return serde_json::json!({
                "content": [{"type": "text", "text": serde_json::to_string(&result).unwrap_or_default()}]
            });
        }

        // serial_send_command 需要在 lock 外 await，避免与 read_once 死锁
        if name == "serial_send_command" {
            let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let timeout = args.get("timeout").and_then(|v| v.as_f64()).unwrap_or(90.0);
            // fast-path: reboot/shutdown/poweroff 直接发送不等待
            let cmd_trimmed = command.trim();
            if cmd_trimmed == "reboot" || cmd_trimmed == "poweroff" || cmd_trimmed == "shutdown" {
                let mut engine = self.engine.lock().await;
                engine.console.sendline(command);
                engine.console.drain_writes().await;
                return serde_json::json!({
                    "content": [{"type": "text", "text": serde_json::to_string(&serde_json::json!({"output": format!("{cmd_trimmed} sent"), "exit_code": 0, "timed_out": false})).unwrap_or_default()}]
                });
            }
            let rx = {
                let mut engine = self.engine.lock().await;
                engine.queue_command(command, timeout)
            };
            let result = match rx.await {
                Ok(r) => serde_json::json!({"output": r.output, "exit_code": r.exit_code, "timed_out": r.timed_out}),
                Err(_) => serde_json::json!({"error": "Command cancelled"}),
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
                engine.reset_target(wait_boot).await
            }
            "serial_enter_maskrom" => engine.enter_maskrom().await,
            "serial_wait_pattern" => {
                let pattern = args
                    .get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let timeout = args
                    .get("timeout")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(60.0);
                let result = engine.wait_pattern(pattern, timeout).await;
                // 处理 action: send_ctrl_c
                if result["matched"].as_bool().unwrap_or(false)
                    && args
                        .get("action")
                        .and_then(|v| v.as_str())
                        .is_some_and(|a| a == "send_ctrl_c")
                {
                    engine.console.sendcontrol('c');
                }
                result
            }
            "serial_uboot_command" => {
                let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let timeout = args.get("timeout").and_then(|v| v.as_f64()).unwrap_or(15.0);
                engine.send_uboot_command(command, timeout).await
            }
            "serial_new_log" => engine.rotate_log(),
            "serial_poll_logs" => engine.poll_logs(),
            "serial_get_config" => engine.get_config(),
            "serial_claim" => engine.claim_serial().await,
            "serial_load_reference" => {
                let path_str = args.get("reference_log_path").and_then(|v| v.as_str()).unwrap_or("");
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
            },
            "serial_get_stages" => {
                match &engine.detector.learner {
                    Some(learner) => {
                        let fps: Vec<serde_json::Value> = learner.export_fingerprints()
                            .into_iter()
                            .map(|(stage, anchor)| serde_json::json!({"stage": stage, "anchor": anchor}))
                            .collect();
                        serde_json::json!({"fingerprints": fps, "count": fps.len()})
                    },
                    None => serde_json::json!({"fingerprints": [], "count": 0, "message": "No reference loaded. Use serial_load_reference first."}),
                }
            },
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

    fn build_resource_content(
        engine: &crate::serial_engine::SerialEngine,
        uri: &str,
    ) -> Value {
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
            _ => serde_json::json!({
                "description": format!("Unknown prompt: {name}"),
                "messages": []
            }),
        }
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
        values.insert("LOCK_DIR".into(), "/tmp/embedded-debug-test-locks".into());
        values.insert("LOGIN_USER".into(), "root".into());
        values.insert("LOGIN_PASS".into(), "".into());
        values.insert("UBOOT_INTERRUPT_STRATEGY".into(), "lava".into());

        let config = Config {
            values,
            config_path: None,
            project_dir: Some(tmp.path().to_path_buf()),
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
        assert_eq!(result["serverInfo"]["name"], "embedded-debug-mcp");
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
        assert_eq!(tools.len(), 15, "Expected 15 MCP tools");

        // Check some tool names
        let tool_names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
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
