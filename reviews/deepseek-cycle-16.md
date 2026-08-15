# Cycle-16 review: `run_agent_turn_with_state` (crates/kitsu-core/src/orchestrator.rs:865–928)

## 1. State mutation before fallible inner call — error path loses prior state

**Lines 876–879:**
```
876	    state.last_turn = AgentTurnStatus::default();
877	    if let Some(budget) = &cfg.spawn_budget {
878	        budget.reset();
879	    }
```

**Line 885:**
```
885	    let result = run_agent_turn_inner(provider, tools, messages, cfg, &mut turn_usage, state).await;
```

`state.last_turn` is unconditionally overwritten with a fresh default *before* `run_agent_turn_inner` executes. If that call returns `Err`, the previous `state.last_turn` (which a caller might have wanted to inspect) is irretrievably lost. The same applies to the spawn-budget reset on line 878: on error the budget is zeroed with no recovery path. **Recommendation:** capture a snapshot of the prior `state.last_turn` before resetting, and restore it (or log it) on error, or defer the resets into the success path. I hedge: `run_agent_turn_inner` is not visible here, so I cannot assert whether it *itself* overwrites `state.last_turn` before any possible error return; if it does, the reset is harmless — but from this range alone the ordering is risky.

## 2. Event-log reads fields that `run_agent_turn_inner` may not have populated on error

**Lines 887–897:**
```
887	        let completion = match state.last_turn.completion {
888	            TurnCompletion::Complete => "complete",
889	            TurnCompletion::LengthLimited => "length_limited",
890	            TurnCompletion::MaxRounds => "max_rounds",
891	        };
892	        log.record(AgentRuntimeEvent::TurnFinished {
893	            success: result.is_ok(),
894	            completion: completion.to_string(),
895	            tool_errors: state.last_turn.tool_errors,
896	            policy_errors: state.last_turn.policy_errors,
897	        });
```

This block runs regardless of `result` (line 893 correctly reflects success/failure), but it reads `state.last_turn.completion`, `.tool_errors`, and `.policy_errors` — fields whose values are populated *inside* `run_agent_turn_inner`. If that function returns `Err` before fully populating those fields (e.g. a provider-level error before any tool round completes), the log records whatever the `AgentTurnStatus::default()` from line 876 left behind, which may be misleading (a default `completion` variant reported as if the turn finished normally). **Recommendation:** guard the `log.record` call so that on error it omits or explicitly marks the completion/tool/policy fields, or ensure `run_agent_turn_inner` *always* finalises these fields before any `Err` return (which I cannot verify from this range — hedge).

## 3. Accumulated `turn_usage` is not recorded in the TurnFinished event

**Line 875:**
```
875	    let mut turn_usage = kitsu_providers::Usage::default();
```

**Line 885:**
```
885	    let result = run_agent_turn_inner(provider, tools, messages, cfg, &mut turn_usage, state).await;
```

`turn_usage` is accumulated by the inner call and then rendered to the terminal (lines 909–926), but it is *not* included in the structured `TurnFinished` event logged at lines 886–898. Only completion status, success, and error counts are logged; the token counts that the function already computed are silently dropped in the log path. **Recommendation:** add the usage triple (prompt/completion/total tokens) to the `AgentRuntimeEvent::TurnFinished` variant, or emit a separate usage event. I hedge: the `AgentRuntimeEvent` enum is not visible here; it may already carry optional usage fields that this call site simply isn't populating.

## 4. Duplicate `!cfg.quiet` guard blocks can drift

**Lines 901 and 909:**
```
901	    if !cfg.quiet {
902	        if !cfg.stream {
903	            if let Ok(text) = &result {
904	                kitsu_render::print_message(text);
905	            }
906	        }
907	        kitsu_render::finish_line();
908	    }
909	    if !cfg.quiet {
910	        let usage = …
919	        …
925	    }
```

Two consecutive blocks guarded by the identical condition `!cfg.quiet`. They are separated only by a blank line and a comment. If a future edit adds side-effecting code between them (or removes the comment), the second block could accidentally execute under a different guard. **Recommendation:** merge into a single `if !cfg.quiet { … }` block. Low severity — purely a maintainability note.
