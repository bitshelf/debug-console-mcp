//! rmcp ServerHandler adapter — bridges the rmcp SDK to our existing McpServer.
//!
//! The McpServer contains all tool implementations, resource/prompt builders,
//! and task management. This module provides a ServerHandler trait impl
//! that delegates to McpServer, replacing the manual JSON-RPC router in mcp.rs.

use std::sync::Arc;

use rmcp::{
    ErrorData,
    RoleServer,
    ServerHandler,
    model::*,
    service::RequestContext,
};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::mcp::McpServer;
use crate::serial_engine::SharedEngine;
use crate::task_manager::TaskManager;

#[derive(Clone)]
pub struct McpHandler {
    inner: Arc<Mutex<McpServer>>,
    engine: SharedEngine,
    tasks: Arc<TaskManager>,
}

impl McpHandler {
    pub fn new(engine: SharedEngine, tasks: Arc<TaskManager>) -> Self {
        let inner = Arc::new(Mutex::new(McpServer::new(engine.clone(), tasks.clone())));
        Self { inner, engine, tasks }
    }

    pub fn engine(&self) -> &SharedEngine {
        &self.engine
    }
}

impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .enable_tasks()
                .build(),
        )
    }

    // ── Tools ──────────────────────────────────────────────────────────

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let server = self.inner.lock().await;
        let tools: Vec<Tool> = server.tools.iter().map(|t| {
            Tool::new(
                t.name,
                t.description,
                Arc::new(t.input_schema.as_object().cloned().unwrap_or_default()),
            )
        }).collect();
        let mut result = ListToolsResult::default();
        result.tools = tools;
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name;
        let args = serde_json::to_value(request.arguments.unwrap_or_default()).unwrap_or(Value::Null);

        // Guard: block serial tools when dutabo has taken over
        if self.engine.lock().await.state.current()
            == crate::state_manager::TargetState::Dutabo
        {
            let text = serde_json::to_string(&serde_json::json!({
                "success": false,
                "error": "Serial is taken over by dutabo interactive session",
                "state": "dutabo",
            })).unwrap_or_default();
            return Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                ContentBlock::text(text),
            ])));
        }

        let params = serde_json::json!({ "name": name, "arguments": args });
        let mut server = self.inner.lock().await;
        let result = server.handle_call_tool(params).await;

        if McpServer::is_task_response(&result) {
            let mut task = Task::default();
            task.task_id = result["taskId"].as_str().unwrap_or("").to_string();
            task.status = TaskStatus::Working;
            task.status_message = Some(result["statusMessage"].as_str().unwrap_or("").to_string());
            Ok(CallToolResponse::Task(CreateTaskResult::new(task)))
        } else {
            let text = serde_json::to_string(&result).unwrap_or_default();
            Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                ContentBlock::text(text),
            ])))
        }
    }

    // ── Tasks ──────────────────────────────────────────────────────────

    async fn get_task(
        &self,
        request: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        match self.tasks.get(&request.task_id).await {
            Some(result) => Ok(result),
            None => Err(ErrorData::invalid_params(
                format!("Task not found: {}", request.task_id),
                None,
            )),
        }
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.tasks.cancel(&request.task_id).await;
        Ok(())
    }
}
