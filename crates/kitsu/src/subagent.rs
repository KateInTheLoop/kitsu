//! Subagents (design docs/subagents-design.md, Stage A): the `spawn_agent` tool
//! runs a nested, ISOLATED agent loop on a delegated task and returns only the
//! conclusion to the parent — so a big exploration stays out of the main context.
//!
//! The subagent inherits the provider, permissions, tools, and skills, but gets
//! a FRESH message list and its own context budget. It is NOT given the
//! `spawn_agent` tool (depth-1), and its filesystem actions still go through the
//! shared permission model — no privilege escalation.

use std::sync::Arc;

use kitsu_core::{run_agent_turn, AgentConfig};
use kitsu_providers::{ChatMessage, Provider, ToolDescriptor};
use kitsu_tools::{HandlerFn, Registry, ToolError};

#[derive(serde::Deserialize)]
struct SpawnArgs {
    task: String,
}

/// System prompt seeding a subagent's isolated context.
const SUBAGENT_SYSTEM: &str = "You are a kitsu subagent handling ONE delegated task in a fresh, \
isolated context (you cannot see the parent conversation). Use your tools to complete it, then \
reply with a concise, self-contained result the caller can use directly. Do not ask questions — \
do the task with what you have.";

/// Build the `spawn_agent` tool. `sub_tools` is the subagent's registry (base
/// tools only — it must NOT contain `spawn_agent`). `sub_cfg` is the subagent's
/// non-streaming agent config, and `extra_system` seeds inherited context (e.g. a
/// skill index) into the subagent's system prompt.
pub fn spawn_agent_tool<P: Provider + 'static>(
    provider: Arc<P>,
    sub_tools: Arc<Registry>,
    sub_cfg: AgentConfig,
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
        let extra_system = extra_system.clone();
        Box::pin(async move {
            let a: SpawnArgs =
                serde_json::from_str(&args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
            let mut system = SUBAGENT_SYSTEM.to_string();
            if !extra_system.is_empty() {
                system.push_str("\n\n");
                system.push_str(&extra_system);
            }
            let mut messages = vec![ChatMessage::system(system), ChatMessage::user(a.task)];
            // Fresh state each call; the isolated loop runs to completion and only
            // its final answer is returned (child rounds never enter the parent).
            match run_agent_turn(&provider, &sub_tools, &mut messages, &sub_cfg).await {
                Ok(text) if !text.trim().is_empty() => Ok(text),
                Ok(_) => Ok("(subagent returned no result)".to_string()),
                Err(e) => Err(ToolError::ExecError(format!("subagent failed: {e}"))),
            }
        })
    });
    (descriptor, handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use kitsu_providers::{ChatDelta, ChatRequest, ChatStream, FinishReason, ProviderError};
    use std::sync::Mutex;

    /// A provider that returns one scripted round per `chat_stream` call.
    struct MockProvider {
        rounds: Mutex<Vec<Vec<ChatDelta>>>,
    }
    impl Provider for MockProvider {
        async fn healthcheck(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn chat_stream(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
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
        });
        let (desc, handler) = spawn_agent_tool(
            provider,
            Arc::new(Registry::new()),
            sub_cfg(),
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
    async fn spawn_agent_reports_bad_args() {
        let provider = Arc::new(MockProvider {
            rounds: Mutex::new(vec![]),
        });
        let (_d, handler) = spawn_agent_tool(
            provider,
            Arc::new(Registry::new()),
            sub_cfg(),
            String::new(),
        );
        assert!(handler("not json".to_string()).await.is_err());
    }
}
