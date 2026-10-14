//! kitsu-tools — tool registry, policy engine, built-in tools. See design §7.
//! MVP for the agent loop: a registry that validates + runs tool calls and
//! returns a JSON string result (or an error rendered as a tool result).

pub mod builtins;
pub mod error;
pub mod mcp_codec;
pub mod permissions;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub mod rename_flags;
pub mod semantic;

// E06 begins with a private, provider-free admission boundary. Later cards
// consume these values from the bounded child supervisor; they are not a
// public extension API.
mod external;

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
pub use builtins::{ToolTier, LEAN_TIER_WINDOW_THRESHOLD};
pub use error::{RegistryError, ToolError, ToolInterruption};
pub use permissions::{
    host_home_dir, Action, Decision, Mode, PermissionRequest, Permissions, Prompter,
    ReadAuthorization, Revocation, TrustedImplicitReadRoot,
};

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use kitsu_providers::ToolDescriptor;

/// Where a permission gate must take an ungranted decision (design §4.4, B2).
///
/// This is the seam the blocked-agent work is built on: the gates keep asking
/// one question ("may I prompt?"), but the answer now carries *who* would be
/// asked. Nothing routes to the registry yet, so only `Foreground` permits an
/// interactive prompt and both other routes keep failing closed.
#[derive(Debug, Clone)]
pub enum PromptRoute {
    /// The future owns the process's interactive input and may prompt.
    Foreground,
    /// No prompt is reachable: an ungranted operation must fail closed.
    Denied,
    /// A registry-tracked background agent that a human could answer for.
    ///
    /// `sender` is the channel the gate parks its question on; the registry
    /// (T2-S4) owns the receiver. `kitsu-tools` cannot name a `kitsu` type, so
    /// the agent is still identified by a bare id.
    Registry {
        agent_id: u64,
        sender: PromptRequestSender,
    },
}

/// What a parked gate is asking about. Both gates travel the same channel and
/// share one answer vocabulary ([`Decision`]); only the question differs.
#[derive(Debug, Clone)]
pub enum PromptRequestPayload {
    /// A path/file gate in `Permissions::resolve`.
    Path(PermissionRequest),
    /// The `CommandMode::Ask` gate in `command.rs`.
    Command(CommandApprovalRequest),
}

/// One ungranted decision a background agent needs a human to take.
///
/// The gate builds exactly the request the foreground `Prompter` would have
/// been shown, sends it, and awaits the answer on `reply`.
#[derive(Debug)]
pub struct PromptRequest {
    /// Registry id of the agent that is now blocked.
    pub agent_id: u64,
    pub payload: PromptRequestPayload,
    /// One-shot answer channel. Dropping it resolves [`Decision::Interrupt`],
    /// which both gates already map to `AuthorityInputClosed`.
    pub reply: tokio::sync::oneshot::Sender<Decision>,
}

/// Channel a `Registry` route parks approval requests on. Unbounded so a gate
/// never blocks the agent's task before it is marked `Blocked`.
pub type PromptRequestSender = tokio::sync::mpsc::UnboundedSender<PromptRequest>;

tokio::task_local! {
    static PROMPT_ROUTE: PromptRoute;
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
    PROMPT_ROUTE.scope(PromptRoute::Denied, future).await
}

/// Run tool work whose ungranted decisions belong to a registry-tracked agent.
///
/// Only the route is recorded here; the gates do not consult it yet, so such a
/// future still fails closed exactly like [`scope_without_interactive_prompts`].
pub async fn scope_with_registry_prompts<F>(
    agent_id: u64,
    sender: PromptRequestSender,
    future: F,
) -> F::Output
where
    F: Future,
{
    PROMPT_ROUTE
        .scope(PromptRoute::Registry { agent_id, sender }, future)
        .await
}

/// The route in effect for the current task; an unscoped task is foreground.
pub(crate) fn prompt_route() -> PromptRoute {
    PROMPT_ROUTE
        .try_with(|route| route.clone())
        .unwrap_or(PromptRoute::Foreground)
}

/// Retained as the single definition of "may this future prompt?" — the gates
/// now match the whole route, so only the route tests still call it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn interactive_prompts_allowed() -> bool {
    matches!(prompt_route(), PromptRoute::Foreground)
}

/// Park one ungranted decision on a [`PromptRoute::Registry`] channel and wait
/// for the human answer.
///
/// A closed channel or a dropped reply sender resolves [`Decision::Interrupt`],
/// which both gates already map to `AuthorityInputClosed`.
pub(crate) async fn ask_registry(
    agent_id: u64,
    sender: &PromptRequestSender,
    payload: PromptRequestPayload,
) -> Decision {
    let (reply, answer) = tokio::sync::oneshot::channel();
    if sender
        .send(PromptRequest {
            agent_id,
            payload,
            reply,
        })
        .is_err()
    {
        return Decision::Interrupt;
    }
    answer.await.unwrap_or(Decision::Interrupt)
}

/// Test-side stand-in for the registry (T2-S4): answers every parked request
/// with `decision` and records the payloads it saw.
///
/// STUB — exists only so T2-S2 can be tested before the registry owns the
/// receiver. Delete when `AgentRegistry::answer` lands.
#[cfg(test)]
pub(crate) fn registry_prompt_stub(
    decision: Decision,
) -> (
    PromptRequestSender,
    Arc<std::sync::Mutex<Vec<PromptRequestPayload>>>,
) {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<PromptRequest>();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Some(request) = receiver.recv().await {
            sink.lock().unwrap().push(request.payload.clone());
            let _ = request.reply.send(decision);
        }
    });
    (sender, seen)
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
    handlers: HashMap<String, Arc<HandlerFn>>,
    descriptors: Vec<ToolDescriptor>,
}

/// One immutable tool inventory frozen from a [`Registry`]. Descriptor order,
/// handler identity, and descriptor-free handlers all match the source at the
/// instant of freezing. Later registry composition cannot change the schemas
/// or call resolution owned by an in-flight provider request.
pub struct ToolInventory {
    handlers: HashMap<String, Arc<HandlerFn>>,
    descriptors: Vec<ToolDescriptor>,
}

impl ToolInventory {
    /// Exact descriptors frozen for this inventory, in registration order.
    pub fn descriptors(&self) -> &[ToolDescriptor] {
        &self.descriptors
    }

    pub fn is_registered(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    pub async fn run(&self, name: &str, args: String) -> Result<String, ToolError> {
        match self.handlers.get(name) {
            Some(handler) => handler(args).await,
            None => Err(ToolError::NotFound(name.to_string())),
        }
    }
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
        self.handlers.insert(name, Arc::new(handler));
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
        self.handlers
            .insert(descriptor.name.clone(), Arc::new(handler));
        self.descriptors.push(descriptor);
        Ok(())
    }

    /// Descriptors advertised to the model. Empty ⇒ orchestrator sends no
    /// `tools` field (preserving the descriptor-free `ask` path).
    pub fn descriptors(&self) -> &[ToolDescriptor] {
        &self.descriptors
    }

    /// Freeze an owned request inventory. Cloning shares immutable handler
    /// identities while copying the bounded schema/name collections, so the
    /// returned value no longer borrows and cannot observe later registration.
    pub fn inventory(&self) -> ToolInventory {
        ToolInventory {
            handlers: self.handlers.clone(),
            descriptors: self.descriptors.clone(),
        }
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

    #[tokio::test]
    async fn frozen_inventory_retains_exact_descriptor_and_handler_snapshot() {
        let mut registry = Registry::new();
        let first = descriptor("first_tool", "first descriptor");
        registry
            .register_with_descriptor(first.clone(), handler("first result"))
            .expect("first registration succeeds");
        registry
            .register("legacy_tool", handler("legacy result"))
            .expect("legacy registration succeeds");

        let frozen = registry.inventory();

        let second = descriptor("second_tool", "second descriptor");
        registry
            .register_with_descriptor(second.clone(), handler("second result"))
            .expect("later registration succeeds");

        assert_eq!(frozen.descriptors(), std::slice::from_ref(&first));
        assert!(frozen.is_registered("first_tool"));
        assert!(frozen.is_registered("legacy_tool"));
        assert!(!frozen.is_registered("second_tool"));
        assert_eq!(
            frozen.run("first_tool", "{}".to_string()).await.unwrap(),
            "first result"
        );
        assert_eq!(
            frozen.run("legacy_tool", "{}".to_string()).await.unwrap(),
            "legacy result"
        );
        assert!(matches!(
            frozen
                .run("second_tool", "{}".to_string())
                .await
                .unwrap_err(),
            ToolError::NotFound(name) if name == "second_tool"
        ));

        let next = registry.inventory();
        assert_eq!(next.descriptors(), &[first, second]);
        assert_eq!(
            next.run("second_tool", "{}".to_string()).await.unwrap(),
            "second result"
        );
    }

    // T2-S1 — the prompt route seam. These pin only the route itself: no gate
    // consults it yet, so a `Registry` route still reports "not foreground"
    // and every existing caller keeps today's behavior.

    #[tokio::test]
    async fn unset_prompt_route_defaults_to_foreground() {
        assert!(matches!(prompt_route(), PromptRoute::Foreground));
        assert!(
            interactive_prompts_allowed(),
            "an unset route must behave like today's unwrap_or(true)"
        );
    }

    #[tokio::test]
    async fn interactive_prompts_allowed_is_exactly_the_foreground_route() {
        fn allowed_matches_route() {
            let route = prompt_route();
            assert_eq!(
                interactive_prompts_allowed(),
                matches!(route, PromptRoute::Foreground),
                "interactive_prompts_allowed() must be exactly `route == Foreground`"
            );
        }

        allowed_matches_route(); // unset == Foreground
        scope_without_interactive_prompts(async { allowed_matches_route() }).await;
        let (sender, _rx) = tokio::sync::mpsc::unbounded_channel();
        scope_with_registry_prompts(7, sender, async { allowed_matches_route() }).await;
    }

    #[tokio::test]
    async fn scope_without_interactive_prompts_sets_the_denied_route() {
        scope_without_interactive_prompts(async {
            assert!(matches!(prompt_route(), PromptRoute::Denied));
            assert!(
                !interactive_prompts_allowed(),
                "the denied route must keep failing closed for every current caller"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn registry_route_carries_the_agent_id_and_is_not_foreground() {
        let (sender, _rx) = tokio::sync::mpsc::unbounded_channel();
        scope_with_registry_prompts(42, sender, async {
            match prompt_route() {
                PromptRoute::Registry { agent_id, .. } => assert_eq!(agent_id, 42),
                _ => panic!("scope_with_registry_prompts must set the Registry route"),
            }
            assert!(
                !interactive_prompts_allowed(),
                "nothing routes yet, so the gates must still fail closed on a Registry route"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn nested_prompt_route_scope_restores_the_outer_route() {
        let (sender, _rx) = tokio::sync::mpsc::unbounded_channel();
        scope_without_interactive_prompts(async {
            scope_with_registry_prompts(9, sender, async {
                assert!(matches!(
                    prompt_route(),
                    PromptRoute::Registry { agent_id: 9, .. }
                ));
            })
            .await;
            assert!(
                matches!(prompt_route(), PromptRoute::Denied),
                "a nested scope must not outlive itself"
            );
        })
        .await;

        assert!(matches!(prompt_route(), PromptRoute::Foreground));
    }

    #[tokio::test]
    async fn prompt_route_does_not_leak_into_a_sibling_task() {
        let (sender, _rx) = tokio::sync::mpsc::unbounded_channel();
        scope_with_registry_prompts(3, sender, async {
            let sibling = tokio::spawn(async { matches!(prompt_route(), PromptRoute::Foreground) });
            assert!(
                sibling.await.expect("sibling task joins"),
                "the route is task-local: a sibling task must see the default"
            );
            assert!(matches!(
                prompt_route(),
                PromptRoute::Registry { agent_id: 3, .. }
            ));
        })
        .await;
    }
}
