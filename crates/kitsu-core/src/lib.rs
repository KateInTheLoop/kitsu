//! kitsu-core — the orchestrator agent loop. This is the heart of kitsu:
//! call model → parse tool calls → run tools → append results → repeat until the
//! model stops or a cap is hit (design §5). Loop BODY is implemented by the
//! Developer agent; the signature + config are pinned here as the contract.

pub mod orchestrator;

pub use orchestrator::{run_agent_turn, AgentConfig, AgentError};
