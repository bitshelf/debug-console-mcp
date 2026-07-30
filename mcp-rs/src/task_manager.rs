//! MCP Tasks Extension (SEP-2663) — server-side async task manager.
//!
//! Uses rmcp 3.0.1 types (`TaskStatus`, `Task`, `DetailedTask`, `CreateTaskResult`)
//! for MCP protocol conformance. Internal task tracking uses its own `Task` struct
//! with `CancellationToken` and `Instant` for runtime management.
//!
//! ## SEP-2663 Methods
//!
//! | Method        | Purpose                        |
//! |---------------|--------------------------------|
//! | `tasks/get`   | Poll status + retrieve result  |
//! | `tasks/cancel`| Cancel an in-progress task     |
//! | `tasks/update`| Submit client input response   |

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;

use rmcp::model::{CreateTaskResult, DetailedTask, GetTaskResult, Task as RmcpTask, TaskPayload, TaskStatus};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

// ── Task ID generation ──────────────────────────────────────────────────────

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

fn new_task_id() -> String {
    let id = NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst);
    format!("task-{}", id)
}

// ── Task types ──────────────────────────────────────────────────────────────

/// What kind of async operation this task represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    ResetAndWait,
    EnterUboot,
    RebootUboot,
    EnterMaskrom,
    PowerCycle,
    Flash,
    WaitPattern,
}

impl TaskKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            TaskKind::ResetAndWait => "reset_and_wait",
            TaskKind::EnterUboot => "enter_uboot",
            TaskKind::RebootUboot => "reboot_uboot",
            TaskKind::EnterMaskrom => "enter_maskrom",
            TaskKind::PowerCycle => "power_cycle",
            TaskKind::Flash => "flash",
            TaskKind::WaitPattern => "wait_pattern",
        }
    }
}

// ── Task (internal) ──────────────────────────────────────────────────────────

/// Internal task tracked by the MCP server. Converts to rmcp types at the boundary.
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub status_message: String,
    pub progress: Option<TaskProgress>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub cancel_token: CancellationToken,
    pub created_at: Instant,
    pub dut_alias: Option<String>,
}

/// Progress info for a running task.
#[derive(Debug, Clone, Serialize)]
pub struct TaskProgress {
    /// Current boot stage name (e.g. "DDR", "SPL", "U-Boot")
    pub stage: Option<String>,
    /// 0.0–1.0 estimate
    pub percent: Option<f32>,
    /// Human-readable description
    pub message: Option<String>,
}

// ── TaskManager ─────────────────────────────────────────────────────────────

/// Manages all async tasks for the MCP server.
///
/// Thread-safe — wrapped in `Arc<TaskManager>` and shared between the MCP
/// server and background task executors.
pub struct TaskManagerInner {
    tasks: HashMap<String, Task>,
    /// Per-DUT concurrency: at most 1 task per DUT at a time.
    by_dut: HashMap<String, String>,
}

pub struct TaskManager {
    inner: Mutex<TaskManagerInner>,
}

impl TaskManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(TaskManagerInner {
                tasks: HashMap::new(),
                by_dut: HashMap::new(),
            }),
        })
    }

    /// Create a new task and return its ID. Returns `None` if the DUT already
    /// has a running task (enforcing per-DUT serialization).
    pub async fn create(
        &self,
        kind: TaskKind,
        dut_alias: Option<String>,
        status_message: String,
    ) -> Option<String> {
        let mut inner = self.inner.lock().await;

        // Per-DUT concurrency: only 1 task per DUT at a time
        if let Some(ref alias) = dut_alias {
            if let Some(existing_id) = inner.by_dut.get(alias) {
                if let Some(task) = inner.tasks.get(existing_id) {
                    if !task.status.is_terminal() {
                        tracing::warn!(
                            task_id = %existing_id,
                            dut = %alias,
                            "Task creation rejected: DUT already has running task"
                        );
                        return None;
                    }
                }
            }
        }

        let id = new_task_id();
        let task = Task {
            id: id.clone(),
            kind,
            status: TaskStatus::Working,
            status_message,
            progress: None,
            result: None,
            error: None,
            cancel_token: CancellationToken::new(),
            created_at: Instant::now(),
            dut_alias: dut_alias.clone(),
        };

        if let Some(alias) = dut_alias {
            inner.by_dut.insert(alias, id.clone());
        }
        inner.tasks.insert(id.clone(), task);
        tracing::info!(task_id = %id, kind = %kind.display_name(), "Task created");
        Some(id)
    }

    /// Get a task by ID and return rmcp `GetTaskResult`.
    pub async fn get(&self, task_id: &str) -> Option<GetTaskResult> {
        let inner = self.inner.lock().await;
        inner.tasks.get(task_id).map(|t| t.to_get_task_result())
    }

    /// Get a task by ID and return rmcp `CreateTaskResult` (for tool responses).
    pub async fn get_create_result(&self, task_id: &str) -> Option<CreateTaskResult> {
        let inner = self.inner.lock().await;
        inner.tasks.get(task_id).map(|t| t.to_create_task_result())
    }

    /// Update task progress (running → update stage/percent/message).
    pub async fn update_progress(&self, task_id: &str, progress: TaskProgress) {
        let mut inner = self.inner.lock().await;
        if let Some(task) = inner.tasks.get_mut(task_id) {
            task.progress = Some(progress);
        }
    }

    /// Mark task as completed with a result.
    pub async fn complete(&self, task_id: &str, result: Value) {
        let mut inner = self.inner.lock().await;
        if let Some(task) = inner.tasks.get_mut(task_id) {
            task.status = TaskStatus::Completed;
            task.result = Some(result);
            task.progress = Some(TaskProgress {
                stage: None,
                percent: Some(1.0),
                message: Some("Complete".into()),
            });
            tracing::info!(task_id = %task_id, "Task completed");
        }
    }

    /// Mark task as failed with an error message.
    pub async fn fail(&self, task_id: &str, error: String) {
        let mut inner = self.inner.lock().await;
        if let Some(task) = inner.tasks.get_mut(task_id) {
            task.status = TaskStatus::Failed;
            task.error = Some(error.clone());
            tracing::warn!(task_id = %task_id, %error, "Task failed");
        }
    }

    /// Cancel a task. Returns true if the task was found and not yet terminal.
    pub async fn cancel(&self, task_id: &str) -> bool {
        let mut inner = self.inner.lock().await;
        if let Some(task) = inner.tasks.get_mut(task_id) {
            if task.status.is_terminal() {
                return false;
            }
            task.cancel_token.cancel();
            task.status = TaskStatus::Cancelled;
            tracing::info!(task_id = %task_id, "Task cancelled");
            true
        } else {
            false
        }
    }

    /// Get the cancellation token for a task (for the executor to poll).
    pub async fn cancel_token(&self, task_id: &str) -> Option<CancellationToken> {
        let inner = self.inner.lock().await;
        inner.tasks.get(task_id).map(|t| t.cancel_token.clone())
    }

    /// Clean up old terminal tasks (older than `max_age` seconds).
    pub async fn cleanup(&self, max_age_secs: u64) {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        let to_remove: Vec<String> = inner
            .tasks
            .iter()
            .filter(|(_, t)| {
                t.status.is_terminal()
                    && now.duration_since(t.created_at).as_secs() > max_age_secs
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &to_remove {
            if let Some(task) = inner.tasks.remove(id) {
                if let Some(alias) = &task.dut_alias {
                    inner.by_dut.remove(alias);
                }
            }
        }
        if !to_remove.is_empty() {
            tracing::debug!(count = to_remove.len(), "Cleaned up old tasks");
        }
    }

    /// List all non-terminal tasks (for debugging/observation).
    pub async fn list_active(&self) -> Vec<DetailedTask> {
        let inner = self.inner.lock().await;
        inner
            .tasks
            .values()
            .filter(|t| !t.status.is_terminal())
            .map(|t| t.to_detailed_task())
            .collect()
    }
}

// ── rmcp type conversions ───────────────────────────────────────────────────

impl Task {
    fn timestamp_iso(&self) -> String {
        // Convert Instant to ISO 8601 using elapsed from now
        // This is approximate — for accurate timestamps we'd need chrono
        let now = Instant::now();
        let elapsed = now.duration_since(self.created_at);
        // Use a simple approach: format as "now - elapsed"
        Utc::now()
            .checked_sub_signed(chrono::TimeDelta::from_std(elapsed).unwrap_or_default())
            .unwrap_or(Utc::now())
            .to_rfc3339()
    }

    fn to_rmcp_task(&self) -> RmcpTask {
        let ts = self.timestamp_iso();
        let mut t = RmcpTask::new(
            self.id.clone(),
            self.status,
            ts.clone(),
            ts,
        );
        if !self.status_message.is_empty() {
            t.status_message = Some(self.status_message.clone());
        }
        t
    }

    fn to_detailed_task(&self) -> DetailedTask {
        let task = self.to_rmcp_task();
        let status = self.status;
        let payload = match status {
            TaskStatus::Working | TaskStatus::InputRequired => TaskPayload::Working,
            TaskStatus::Completed => {
                if let Some(ref result) = self.result {
                    TaskPayload::Completed {
                        result: result.as_object().cloned().unwrap_or_default(),
                    }
                } else {
                    TaskPayload::Completed {
                        result: Default::default(),
                    }
                }
            }
            TaskStatus::Failed => {
                if let Some(ref error) = self.error {
                    let mut err_map = serde_json::Map::new();
                    err_map.insert("message".into(), serde_json::Value::String(error.clone()));
                    TaskPayload::Failed { error: err_map }
                } else {
                    TaskPayload::Failed { error: Default::default() }
                }
            }
            TaskStatus::Cancelled => TaskPayload::Cancelled,
            _ => TaskPayload::Working, // non-exhaustive enum guard
        };
        DetailedTask::new(task, payload)
    }

    fn to_get_task_result(&self) -> GetTaskResult {
        GetTaskResult::new(self.to_detailed_task())
    }

    fn to_create_task_result(&self) -> CreateTaskResult {
        CreateTaskResult::new(self.to_rmcp_task())
    }
}
