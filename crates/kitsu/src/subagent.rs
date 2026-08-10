//! Subagents (design docs/subagents-design.md, Stage A): the `spawn_agent` tool
//! runs a nested, ISOLATED agent loop on a delegated task and returns only the
//! conclusion to the parent — so a big exploration stays out of the main context.
//!
//! The subagent inherits the provider, permissions, tools, and skills, but gets
//! a FRESH message list and its own context budget. It is NOT given the
//! `spawn_agent` tool (depth-1), and its filesystem actions still go through the
//! shared permission model — no privilege escalation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use kitsu_core::{run_agent_turn, AgentConfig};
use kitsu_providers::{ChatMessage, Provider, ToolDescriptor};
use kitsu_tools::{HandlerFn, Registry, ToolError};

#[derive(serde::Deserialize)]
struct SpawnArgs {
    task: String,
}

#[derive(serde::Deserialize)]
struct SpawnManyArgs {
    tasks: Vec<String>,
}

#[derive(serde::Deserialize)]
struct ResultArgs {
    id: u64,
}

/// Cap on concurrent subagents in one `spawn_agents` call, so the model can't
/// fan out unboundedly against the gateway. Extra tasks beyond this are still run
/// — just not all at once (see `buffer` below).
const MAX_PARALLEL: usize = 8;

/// The live `(model, context_window)` a subagent should use, shared with the
/// chat loop so `/model` (which changes the model and re-discovers the window)
/// propagates to subagents instead of them running the session's initial model
/// against a stale window.
pub type LiveModelWindow = Arc<Mutex<(String, Option<u64>)>>;

/// System prompt seeding a subagent's isolated context.
const SUBAGENT_SYSTEM: &str = "You are a kitsu subagent handling ONE delegated task in a fresh, \
isolated context (you cannot see the parent conversation). Use your tools to complete it, then \
reply with a concise, self-contained result the caller can use directly. Do not ask questions — \
do the task with what you have.";

/// Status of a background subagent (Stage C).
pub enum AgentStatus {
    Running,
    Done(String),
    Failed(String),
}

struct AgentRecord {
    task: String,
    status: AgentStatus,
    handle: Option<tokio::task::AbortHandle>,
}

/// Registry of background subagents, shared between `spawn_agent_bg`,
/// `agent_result`, and the `/agents` command. Detached subagents update their
/// record on completion; the user (via `/agents`) or the model (via
/// `agent_result`) reads status/results back.
#[derive(Default)]
pub struct AgentRegistry {
    agents: Mutex<HashMap<u64, AgentRecord>>,
    next: AtomicU64,
}

impl AgentRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a new running agent, returning its id.
    fn start(&self, task: String) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.agents.lock().expect("agent registry").insert(
            id,
            AgentRecord {
                task,
                status: AgentStatus::Running,
                handle: None,
            },
        );
        id
    }

    /// Mark an agent done/failed with its result.
    fn finish(&self, id: u64, result: Result<String, String>) {
        if let Some(rec) = self.agents.lock().expect("agent registry").get_mut(&id) {
            rec.status = match result {
                Ok(r) => AgentStatus::Done(r),
                Err(e) => AgentStatus::Failed(e),
            };
            rec.handle = None;
        }
    }

    fn attach(&self, id: u64, handle: tokio::task::AbortHandle) {
        if let Some(rec) = self.agents.lock().expect("agent registry").get_mut(&id) {
            if matches!(rec.status, AgentStatus::Running) {
                rec.handle = Some(handle);
            }
        }
    }

    /// Highest id allocated before a parent turn starts.
    pub fn watermark(&self) -> u64 {
        self.next.load(Ordering::Relaxed)
    }

    /// Abort detached agents created by a parent turn that is being rolled
    /// back. Agents that predate the turn are intentionally left alone.
    pub fn cancel_started_after(&self, watermark: u64) -> Vec<u64> {
        let mut agents = self.agents.lock().expect("agent registry");
        let mut cancelled = Vec::new();
        for (id, record) in agents.iter_mut() {
            if *id > watermark && matches!(record.status, AgentStatus::Running) {
                if let Some(handle) = record.handle.take() {
                    handle.abort();
                }
                record.status = AgentStatus::Failed(
                    "cancelled because the parent turn was rolled back".to_string(),
                );
                cancelled.push(*id);
            }
        }
        cancelled.sort_unstable();
        cancelled
    }

    /// Count of still-running background agents (for a prompt hint).
    pub fn running(&self) -> usize {
        self.agents
            .lock()
            .expect("agent registry")
            .values()
            .filter(|r| matches!(r.status, AgentStatus::Running))
            .count()
    }

    /// Clear completed records at a session boundary. Callers first ensure no
    /// agents are running, so results from one conversation cannot be fetched
    /// by the next conversation through `agent_result`.
    pub fn reset(&self) {
        let mut agents = self.agents.lock().expect("agent registry");
        debug_assert!(!agents
            .values()
            .any(|record| matches!(record.status, AgentStatus::Running)));
        agents.clear();
        self.next.store(0, Ordering::Relaxed);
    }

    /// One line per agent (`#id  [status]  task`), id-ordered, for `/agents`.
    pub fn list(&self) -> String {
        let g = self.agents.lock().expect("agent registry");
        if g.is_empty() {
            return "no background agents this session".to_string();
        }
        let mut ids: Vec<&u64> = g.keys().collect();
        ids.sort();
        ids.iter()
            .map(|id| {
                let rec = &g[id];
                let tag = match &rec.status {
                    AgentStatus::Running => "running",
                    AgentStatus::Done(_) => "done",
                    AgentStatus::Failed(_) => "failed",
                };
                format!("#{id}  [{tag}]  {}", rec.task)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The full result/status text for one agent, or `None` if unknown.
    pub fn result(&self, id: u64) -> Option<String> {
        let g = self.agents.lock().expect("agent registry");
        g.get(&id).map(|rec| match &rec.status {
            AgentStatus::Running => format!("agent #{id} is still running"),
            AgentStatus::Done(r) => r.clone(),
            AgentStatus::Failed(e) => format!("agent #{id} failed: {e}"),
        })
    }
}

/// Snapshot the current model + window into a runnable config (so a mid-session
/// `/model` applies to every subagent spawned afterwards).
fn resolve_cfg(template: &AgentConfig, live: &LiveModelWindow) -> AgentConfig {
    let mut cfg = template.clone();
    let (model, window) = live.lock().expect("live model/window").clone();
    cfg.model = model;
    cfg.context_window = window;
    cfg
}

/// Run one subagent to completion in its own fresh context and return its final
/// text (or an error string). Shared by `spawn_agent` and `spawn_agents`.
async fn run_subagent<P: Provider>(
    provider: &Arc<P>,
    sub_tools: &Arc<Registry>,
    cfg: &AgentConfig,
    extra_system: &str,
    task: String,
) -> Result<String, String> {
    let mut system = SUBAGENT_SYSTEM.to_string();
    if !extra_system.is_empty() {
        system.push_str("\n\n");
        system.push_str(extra_system);
    }
    let mut messages = vec![ChatMessage::system(system), ChatMessage::user(task)];
    match run_agent_turn(provider, sub_tools, &mut messages, cfg).await {
        Ok(text) if !text.trim().is_empty() => Ok(text),
        Ok(_) => Ok("(subagent returned no result)".to_string()),
        Err(e) => Err(format!("subagent failed: {e}")),
    }
}

/// A short one-line label for a task, for the collected `spawn_agents` output.
fn task_preview(task: &str) -> String {
    let first = task.lines().next().unwrap_or("").trim();
    let short: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        format!("{short}…")
    } else {
        short
    }
}

/// Build the `spawn_agent` tool. `sub_tools` is the subagent's registry (base
/// tools only — it must NOT contain `spawn_agent`). `sub_cfg` is the subagent's
/// non-streaming agent config *template*; its `model` and `context_window` are
/// overwritten at call time from `live` so subagents track `/model`. `extra_system`
/// seeds inherited context (e.g. a skill index) into the subagent's system prompt.
pub fn spawn_agent_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "spawn_agent".to_string(),
        description: "Delegate a self-contained sub-task to a fresh subagent that has its own \
                      isolated context and the same file tools and skills. Use it to keep a large \
                      exploration (reading many files, research) out of the main conversation — \
                      only the subagent's result comes back. Give a complete, standalone task."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {"type": "string", "description": "A complete, self-contained task for the subagent"}
            },
            "required": ["task"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let provider = provider.clone();
        let sub_tools = sub_tools.clone();
        let sub_cfg = sub_cfg.clone();
        let live = live.clone();
        let extra_system = extra_system.clone();
        Box::pin(async move {
            let a: SpawnArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            let cfg = resolve_cfg(&sub_cfg, &live);
            // Fresh state each call; the isolated loop runs to completion and only
            // its final answer is returned (child rounds never enter the parent).
            run_subagent(&provider, &sub_tools, &cfg, &extra_system, a.task)
                .await
                .map_err(ToolError::ExecError)
        })
    });
    (descriptor, handler)
}

/// Build the `spawn_agents` tool: run SEVERAL isolated subagents concurrently and
/// return their collected results (Stage B). Each subagent has its own fresh
/// context, so they run in parallel safely; concurrency is capped at
/// [`MAX_PARALLEL`]. The subagent registry must NOT contain `spawn_agent` /
/// `spawn_agents` (depth-1). One failing subagent does not fail the others.
pub fn spawn_agents_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "spawn_agents".to_string(),
        description:
            "Delegate SEVERAL independent sub-tasks at once, each to its own isolated \
                      subagent, run in parallel. Returns all results labelled by task. Use it to \
                      fan out independent work (e.g. review several files) — only the results come \
                      back, not each subagent's steps. Pass an array of complete, standalone tasks."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Independent, self-contained tasks (each runs in its own subagent)"
                }
            },
            "required": ["tasks"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let provider = provider.clone();
        let sub_tools = sub_tools.clone();
        let sub_cfg = sub_cfg.clone();
        let live = live.clone();
        let extra_system = extra_system.clone();
        Box::pin(async move {
            let a: SpawnManyArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            if a.tasks.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "tasks must be non-empty".to_string(),
                ));
            }
            // Same model/window snapshot for the whole batch.
            let cfg = resolve_cfg(&sub_cfg, &live);
            // Run each task in its own subagent, at most MAX_PARALLEL at a time.
            let stream = futures::stream::iter(a.tasks.into_iter().enumerate().map(|(i, task)| {
                let provider = provider.clone();
                let sub_tools = sub_tools.clone();
                let cfg = cfg.clone();
                let extra_system = extra_system.clone();
                async move {
                    let preview = task_preview(&task);
                    let r = run_subagent(&provider, &sub_tools, &cfg, &extra_system, task).await;
                    (i, preview, r)
                }
            }));
            let mut results: Vec<(usize, String, Result<String, String>)> = stream
                .buffer_unordered(MAX_PARALLEL)
                .collect::<Vec<_>>()
                .await;
            results.sort_by_key(|(i, _, _)| *i); // stable, task-order output
            let mut out = String::new();
            for (i, preview, r) in results {
                out.push_str(&format!("=== subagent {} — {} ===\n", i + 1, preview));
                match r {
                    Ok(text) => out.push_str(text.trim()),
                    Err(e) => out.push_str(&format!("[{e}]")),
                }
                out.push_str("\n\n");
            }
            Ok(out.trim().to_string())
        })
    });
    (descriptor, handler)
}

/// Build the `spawn_agent_bg` tool: start a subagent that runs DETACHED in the
/// background and return its id immediately (Stage C). The subagent runs quiet
/// (no terminal narration) so it doesn't corrupt the interactive prompt; its
/// result is stored in `registry` and fetched later via `agent_result`/`/agents`.
/// `sub_cfg` must already be quiet. Depth-1: the subagent's registry has no spawn
/// tools.
pub fn spawn_agent_bg_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
    live: LiveModelWindow,
    extra_system: String,
    registry: Arc<AgentRegistry>,
) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "spawn_agent_bg".to_string(),
        description: "Start a subagent that runs in the BACKGROUND on a self-contained task and \
                      return its id immediately (it does not block). Use it for long work the user \
                      can keep chatting through; fetch the result later with agent_result(id), or \
                      the user can run /agents. Give a complete, standalone task."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {"type": "string", "description": "A complete, self-contained task for the background subagent"}
            },
            "required": ["task"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let provider = provider.clone();
        let sub_tools = sub_tools.clone();
        let sub_cfg = sub_cfg.clone();
        let live = live.clone();
        let extra_system = extra_system.clone();
        let registry = registry.clone();
        Box::pin(async move {
            let a: SpawnArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            let cfg = resolve_cfg(&sub_cfg, &live);
            let id = registry.start(task_preview(&a.task));
            // Detach: the subagent runs on the runtime while the parent continues.
            let running_registry = registry.clone();
            let handle = tokio::spawn(async move {
                let r = run_subagent(&provider, &sub_tools, &cfg, &extra_system, a.task).await;
                running_registry.finish(id, r);
            });
            registry.attach(id, handle.abort_handle());
            Ok(format!(
                "Started background agent #{id}. It runs detached — fetch its result later with \
                 agent_result({id}); the user can also run /agents to see status."
            ))
        })
    });
    (descriptor, handler)
}

/// Build the `agent_result` tool: fetch a background agent's status or result by
/// id (returns "still running" until it finishes).
pub fn agent_result_tool(registry: Arc<AgentRegistry>) -> (ToolDescriptor, HandlerFn) {
    let descriptor = ToolDescriptor {
        name: "agent_result".to_string(),
        description: "Fetch the status or result of a background subagent started with \
                      spawn_agent_bg, by its id. Returns 'still running' until it finishes."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "integer", "description": "The background agent id"}
            },
            "required": ["id"]
        }),
    };
    let handler: HandlerFn = Box::new(move |args: String| {
        let registry = registry.clone();
        Box::pin(async move {
            let a: ResultArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            registry
                .result(a.id)
                .ok_or_else(|| ToolError::ExecError(format!("no background agent #{}", a.id)))
        })
    });
    (descriptor, handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use kitsu_providers::{ChatDelta, ChatRequest, ChatStream, FinishReason, ProviderError, Role};
    use std::sync::Mutex;

    /// A provider that echoes each request's task (last user message) back as the
    /// answer, so a batch of subagents produces per-task-distinguishable results.
    struct EchoProvider;
    impl Provider for EchoProvider {
        async fn healthcheck(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
            let task = req
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .map(|m| m.content.clone())
                .unwrap_or_default();
            Ok(Box::pin(stream::iter(vec![
                Ok(ChatDelta::Text(format!("echo:{task}"))),
                Ok(ChatDelta::Done(FinishReason::Stop)),
            ])))
        }
        async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
            Ok(None)
        }
    }

    /// A provider that returns one scripted round per `chat_stream` call and
    /// records the model of the last request it saw.
    struct MockProvider {
        rounds: Mutex<Vec<Vec<ChatDelta>>>,
        last_model: Mutex<Option<String>>,
    }
    impl Provider for MockProvider {
        async fn healthcheck(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn chat_stream(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
            *self.last_model.lock().unwrap() = Some(req.model.clone());
            let mut r = self.rounds.lock().unwrap();
            let script = if r.is_empty() {
                Vec::new()
            } else {
                r.remove(0)
            };
            Ok(Box::pin(stream::iter(
                script.into_iter().map(Ok::<_, ProviderError>),
            )))
        }
        async fn list_models(&self) -> Result<Option<Vec<String>>, ProviderError> {
            Ok(None)
        }
    }

    fn sub_cfg() -> AgentConfig {
        AgentConfig {
            model: "m".to_string(),
            stream: false,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn spawn_agent_runs_nested_loop_and_returns_only_the_child_answer() {
        // The child's single round returns text; that text is the tool result.
        let provider = Arc::new(MockProvider {
            rounds: Mutex::new(vec![vec![
                ChatDelta::Text("SUBAGENT_RESULT_42".to_string()),
                ChatDelta::Done(FinishReason::Stop),
            ]]),
            last_model: Mutex::new(None),
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (desc, handler) = spawn_agent_tool(
            provider,
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
        );
        assert_eq!(desc.name, "spawn_agent");
        let out = handler(r#"{"task":"do the thing"}"#.to_string())
            .await
            .expect("subagent should succeed");
        assert!(
            out.contains("SUBAGENT_RESULT_42"),
            "returns the child's answer"
        );
        // Isolation: the returned value is exactly the child's conclusion, not a
        // transcript of its rounds.
        assert!(
            !out.contains("do the thing"),
            "no parent/child transcript leaks"
        );
    }

    #[tokio::test]
    async fn spawn_agent_uses_live_model_at_call_time() {
        // Regression for the frozen-config defect: a `/model` switch after the
        // tool is built must reach the subagent.
        let provider = Arc::new(MockProvider {
            rounds: Mutex::new(vec![vec![
                ChatDelta::Text("ok".to_string()),
                ChatDelta::Done(FinishReason::Stop),
            ]]),
            last_model: Mutex::new(None),
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("model-A".to_string(), Some(128_000))));
        let (_d, handler) = spawn_agent_tool(
            provider.clone(),
            Arc::new(Registry::new()),
            sub_cfg(),
            live.clone(),
            String::new(),
        );
        // Switch the model as `/model` would, AFTER the tool was built.
        *live.lock().unwrap() = ("model-B".to_string(), Some(8_000));
        let _ = handler(r#"{"task":"x"}"#.to_string()).await.unwrap();
        assert_eq!(
            provider.last_model.lock().unwrap().as_deref(),
            Some("model-B"),
            "subagent used the live model, not the frozen initial one"
        );
    }

    #[tokio::test]
    async fn spawn_agents_runs_all_tasks_and_labels_results_in_order() {
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (desc, handler) = spawn_agents_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
        );
        assert_eq!(desc.name, "spawn_agents");
        let out = handler(r#"{"tasks":["alpha","beta","gamma"]}"#.to_string())
            .await
            .expect("batch should succeed");
        // Every task ran and its own result came back.
        assert!(out.contains("echo:alpha"), "alpha ran");
        assert!(out.contains("echo:beta"), "beta ran");
        assert!(out.contains("echo:gamma"), "gamma ran");
        // Results are labelled and emitted in task order despite concurrent run.
        assert!(out.contains("subagent 1 — alpha"));
        assert!(out.contains("subagent 3 — gamma"));
        assert!(
            out.find("subagent 1").unwrap() < out.find("subagent 3").unwrap(),
            "stable task-order output"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_agent_bg_registers_runs_detached_and_stores_result() {
        use std::time::Duration;
        let registry = AgentRegistry::new();
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let mut cfg = sub_cfg();
        cfg.quiet = true;
        let (desc, handler) = spawn_agent_bg_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            cfg,
            live,
            String::new(),
            registry.clone(),
        );
        assert_eq!(desc.name, "spawn_agent_bg");
        // Returns immediately with an id (does not block on the subagent).
        let out = handler(r#"{"task":"do work"}"#.to_string())
            .await
            .expect("spawn should return an id");
        assert!(out.contains("#1"), "returned the agent id: {out}");

        // The detached subagent completes shortly; poll the registry for it.
        let mut final_result = None;
        for _ in 0..200 {
            let r = registry.result(1).expect("agent #1 exists");
            if !r.contains("still running") {
                final_result = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let r = final_result.expect("background agent should finish");
        assert!(r.contains("echo:do work"), "stored result: {r}");
        assert_eq!(registry.running(), 0, "no agents still running");

        // agent_result tool fetches it by id; unknown id errors.
        let (_d, rh) = agent_result_tool(registry.clone());
        let got = rh(r#"{"id":1}"#.to_string()).await.unwrap();
        assert!(got.contains("echo:do work"));
        assert!(rh(r#"{"id":999}"#.to_string()).await.is_err());
        // /agents-style listing shows the finished agent.
        assert!(registry.list().contains("#1"));
        assert!(registry.list().contains("[done]"));
    }

    #[tokio::test]
    async fn rollback_cancels_only_agents_started_after_watermark() {
        let registry = AgentRegistry::new();
        let old = registry.start("older work".to_string());
        let old_handle = tokio::spawn(std::future::pending::<()>());
        registry.attach(old, old_handle.abort_handle());
        let watermark = registry.watermark();

        let new = registry.start("rolled-back work".to_string());
        let new_handle = tokio::spawn(std::future::pending::<()>());
        registry.attach(new, new_handle.abort_handle());

        assert_eq!(registry.cancel_started_after(watermark), vec![new]);
        assert!(registry.result(old).unwrap().contains("still running"));
        assert!(registry
            .result(new)
            .unwrap()
            .contains("parent turn was rolled back"));
        old_handle.abort();
    }

    #[tokio::test]
    async fn spawn_agents_rejects_empty_tasks() {
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (_d, handler) = spawn_agents_tool(
            Arc::new(EchoProvider),
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
        );
        assert!(handler(r#"{"tasks":[]}"#.to_string()).await.is_err());
    }

    #[tokio::test]
    async fn spawn_agent_reports_bad_args() {
        let provider = Arc::new(MockProvider {
            rounds: Mutex::new(vec![]),
            last_model: Mutex::new(None),
        });
        let live: LiveModelWindow = Arc::new(Mutex::new(("m".to_string(), None)));
        let (_d, handler) = spawn_agent_tool(
            provider,
            Arc::new(Registry::new()),
            sub_cfg(),
            live,
            String::new(),
        );
        assert!(handler("not json".to_string()).await.is_err());
    }
}
