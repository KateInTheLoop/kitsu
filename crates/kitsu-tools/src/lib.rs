//! kitsu-tools — tool registry, policy engine, built-in tools. See design §7.
//! MVP for the agent loop: a registry that validates + runs tool calls and
//! returns a JSON string result (or an error rendered as a tool result).

pub mod error;

pub use error::ToolError;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// A tool handler: takes raw JSON args, returns a JSON string result.
pub type HandlerFn = Box<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send>> + Send + Sync,
>;

/// Minimal registry. The orchestrator calls [`Registry::run`] with a tool name
/// and raw args; a missing tool or a handler error becomes a tool-result error
/// (the orchestrator serializes it back to the model — design §5).
#[derive(Default)]
pub struct Registry {
    handlers: HashMap<String, HandlerFn>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: impl Into<String>, handler: HandlerFn) {
        self.handlers.insert(name.into(), handler);
    }

    pub fn is_registered(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    pub async fn run(&self, name: &str, args: String) -> Result<String, ToolError> {
        match self.handlers.get(name) {
            Some(h) => h(args).await,
            None => Err(ToolError::NotFound(name.to_string())),
        }
    }
}
