//! kitsu-tools — tool registry, policy engine, built-in tools. See design §7.
//! MVP for the agent loop: a registry that validates + runs tool calls and
//! returns a JSON string result (or an error rendered as a tool result).

pub mod builtins;
pub mod error;
pub mod permissions;

pub use builtins::command::{
    command_exec_tool, execute_command, scope_command_progress, CommandAmbientAuthority,
    CommandApprovalRequest, CommandArtifactCompletion, CommandArtifactRequest, CommandDecision,
    CommandExecArgs, CommandExecutableFingerprint, CommandExecutableIdentity,
    CommandExecutableResolution, CommandGrantInfo, CommandGrantedExecutable, CommandLimits,
    CommandMode, CommandOutput, CommandOutputArtifact, CommandOutputArtifactSink,
    CommandOutputArtifactWriter, CommandOutputStream, CommandPolicy, CommandProgressSink,
    CommandPrompter, CommandTermination,
};
pub use error::ToolError;
pub use permissions::{
    Action, Decision, Mode, PermissionRequest, Permissions, Prompter, ReadAuthorization,
    Revocation, TrustedImplicitReadRoot,
};

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use kitsu_providers::ToolDescriptor;

tokio::task_local! {
    static INTERACTIVE_PROMPTS_ALLOWED: bool;
}

/// Run tool work without allowing it to claim the process's interactive input.
///
/// Background agents share the foreground permission policies and therefore
/// retain explicit/session grants, but an ungranted operation must fail closed
/// instead of racing the foreground prompt for stdin. Tokio task-local state is
/// intentionally scoped to the agent future and is not a process-global mode.
pub async fn scope_without_interactive_prompts<F>(future: F) -> F::Output
where
    F: Future,
{
    INTERACTIVE_PROMPTS_ALLOWED.scope(false, future).await
}

pub(crate) fn interactive_prompts_allowed() -> bool {
    INTERACTIVE_PROMPTS_ALLOWED
        .try_with(|allowed| *allowed)
        .unwrap_or(true)
}

/// A tool handler: takes raw JSON args, returns a JSON string result.
pub type HandlerFn = Box<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send>> + Send + Sync,
>;

/// Minimal registry. The orchestrator calls [`Registry::run`] with a tool name
/// and raw args; a missing tool or a handler error becomes a tool-result error
/// (the orchestrator serializes it back to the model — design §5).
///
/// Tools registered with a [`ToolDescriptor`] (via [`Registry::register_with_descriptor`])
/// are advertised to the model; tools added via the legacy [`Registry::register`]
/// have a handler but no descriptor (used by tests and the descriptor-free `ask` path).
#[derive(Default)]
pub struct Registry {
    handlers: HashMap<String, HandlerFn>,
    descriptors: Vec<ToolDescriptor>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            descriptors: Vec::new(),
        }
    }

    /// Register a handler with no advertised descriptor. The model is not told
    /// this tool exists; used by the `ask` path and orchestrator tests.
    pub fn register(&mut self, name: impl Into<String>, handler: HandlerFn) {
        self.handlers.insert(name.into(), handler);
    }

    /// Register a handler AND advertise its descriptor to the model.
    pub fn register_with_descriptor(&mut self, descriptor: ToolDescriptor, handler: HandlerFn) {
        self.handlers.insert(descriptor.name.clone(), handler);
        self.descriptors.push(descriptor);
    }

    /// Descriptors advertised to the model. Empty ⇒ orchestrator sends no
    /// `tools` field (preserving the descriptor-free `ask` path).
    pub fn descriptors(&self) -> &[ToolDescriptor] {
        &self.descriptors
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
