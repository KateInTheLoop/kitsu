//! kitsu-tools — tool registry, policy engine, built-in tools. See design §7.
//! MVP for the agent loop: a registry that validates + runs tool calls and
//! returns a JSON string result (or an error rendered as a tool result).

pub mod builtins;
pub mod error;
pub mod permissions;

pub use builtins::command::{
    command_exec_tool, env_name_looks_secret, execute_command, scope_command_progress,
    CommandAmbientAuthority, CommandApprovalRequest, CommandArtifactCompletion,
    CommandArtifactRequest, CommandContainmentBackendKind, CommandContainmentConfig,
    CommandContainmentError, CommandContainmentReady, CommandContainmentStatus,
    CommandContainmentUnavailableReason, CommandDecision, CommandExecArgs,
    CommandExecutableFingerprint, CommandExecutableIdentity, CommandExecutableResolution,
    CommandGrantInfo, CommandGrantedExecutable, CommandLimits, CommandMode, CommandOutput,
    CommandOutputArtifact, CommandOutputArtifactSink, CommandOutputArtifactWriter,
    CommandOutputStream, CommandPolicy, CommandProgressSink, CommandPrompter, CommandTermination,
    COMMAND_CONTAINMENT_ENVIRONMENT, DEFAULT_BUBBLEWRAP_PATH, SECRET_LIKE_ENV_NAME_MARKERS,
};
pub use error::{RegistryError, ToolError, ToolInterruption};
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
    /// this tool exists; used by the `ask` path and orchestrator tests. A name
    /// collision is rejected before the existing handler is touched.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        handler: HandlerFn,
    ) -> Result<(), RegistryError> {
        let name = name.into();
        if self.handlers.contains_key(&name) {
            return Err(RegistryError::DuplicateName { name });
        }
        self.handlers.insert(name, handler);
        Ok(())
    }

    /// Register a handler AND advertise its descriptor to the model. A name
    /// collision is rejected before either registry collection is mutated.
    pub fn register_with_descriptor(
        &mut self,
        descriptor: ToolDescriptor,
        handler: HandlerFn,
    ) -> Result<(), RegistryError> {
        if self.handlers.contains_key(&descriptor.name) {
            return Err(RegistryError::DuplicateName {
                name: descriptor.name,
            });
        }
        self.handlers.insert(descriptor.name.clone(), handler);
        self.descriptors.push(descriptor);
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(result: &'static str) -> HandlerFn {
        Box::new(move |_| Box::pin(async move { Ok(result.to_string()) }))
    }

    fn descriptor(name: &str, description: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            description: description.to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    #[tokio::test]
    async fn descriptor_free_duplicate_is_rejected_without_replacing_first_handler() {
        let mut registry = Registry::new();
        registry
            .register("test_tool", handler("first"))
            .expect("first registration succeeds");

        let error = registry
            .register("test_tool", handler("second"))
            .expect_err("duplicate registration fails");

        assert_eq!(
            error,
            RegistryError::DuplicateName {
                name: "test_tool".to_string(),
            }
        );
        assert!(registry.descriptors().is_empty());
        assert_eq!(
            registry.run("test_tool", "{}".to_string()).await.unwrap(),
            "first"
        );
    }

    #[tokio::test]
    async fn described_duplicate_is_rejected_without_replacing_first_entry() {
        let mut registry = Registry::new();
        let first = descriptor("test_tool", "first descriptor");
        registry
            .register_with_descriptor(first.clone(), handler("first"))
            .expect("first registration succeeds");

        let error = registry
            .register_with_descriptor(
                descriptor("test_tool", "second descriptor"),
                handler("second"),
            )
            .expect_err("duplicate registration fails");

        assert_eq!(
            error,
            RegistryError::DuplicateName {
                name: "test_tool".to_string(),
            }
        );
        assert_eq!(registry.descriptors(), &[first]);
        assert_eq!(
            registry.run("test_tool", "{}".to_string()).await.unwrap(),
            "first"
        );
    }

    #[tokio::test]
    async fn descriptor_free_then_described_collision_preserves_handler_only_state() {
        let mut registry = Registry::new();
        registry
            .register("test_tool", handler("descriptor-free"))
            .expect("first registration succeeds");

        let error = registry
            .register_with_descriptor(descriptor("test_tool", "described"), handler("described"))
            .expect_err("cross-path duplicate fails");

        assert_eq!(
            error,
            RegistryError::DuplicateName {
                name: "test_tool".to_string(),
            }
        );
        assert!(registry.descriptors().is_empty());
        assert_eq!(
            registry.run("test_tool", "{}".to_string()).await.unwrap(),
            "descriptor-free"
        );
    }

    #[tokio::test]
    async fn described_then_descriptor_free_collision_preserves_described_state() {
        let mut registry = Registry::new();
        let first = descriptor("test_tool", "described");
        registry
            .register_with_descriptor(first.clone(), handler("described"))
            .expect("first registration succeeds");

        let error = registry
            .register("test_tool", handler("descriptor-free"))
            .expect_err("cross-path duplicate fails");

        assert_eq!(
            error,
            RegistryError::DuplicateName {
                name: "test_tool".to_string(),
            }
        );
        assert_eq!(registry.descriptors(), &[first]);
        assert_eq!(
            registry.run("test_tool", "{}".to_string()).await.unwrap(),
            "described"
        );
    }
}
