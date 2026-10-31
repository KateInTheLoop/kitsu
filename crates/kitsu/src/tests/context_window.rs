use super::*;

#[test]
fn endpoint_display_redacts_userinfo_and_query_values() {
    assert_eq!(
        redact_endpoint("https://user:secret@example.test/v1/chat?api_key=secret"),
        "https://<redacted>@example.test/v1/chat?<redacted>"
    );
    assert_eq!(
        redact_endpoint("http://localhost:11434/v1/chat/completions"),
        "http://localhost:11434/v1/chat/completions"
    );
}

#[test]
fn context_cache_reports_fresh_stale_missing_and_unreadable_states() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("context.json");
    let key = "v2-fnv128-test";
    let mut cache = ContextWindowCache::default();
    cache.entries.insert(key.to_string(), (32_768, 1_000));
    std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();

    let fresh = inspect_cached_context_window_at(&path, key, 1_090);
    assert_eq!(fresh.window, Some(32_768));
    assert_eq!(
        fresh.state,
        ContextWindowCacheState::Fresh {
            age_secs: 90,
            ttl_secs: CONTEXT_WINDOW_CACHE_TTL_SECS,
        }
    );

    let stale = inspect_cached_context_window_at(&path, key, 1_000 + CONTEXT_WINDOW_CACHE_TTL_SECS);
    assert_eq!(stale.window, None);
    assert!(matches!(stale.state, ContextWindowCacheState::Stale { .. }));

    let missing = inspect_cached_context_window_at(&path, "another-key", 1_090);
    assert_eq!(missing.window, None);
    assert_eq!(missing.state, ContextWindowCacheState::Miss);

    std::fs::write(&path, b"not-json").unwrap();
    let unreadable = inspect_cached_context_window_at(&path, key, 1_090);
    assert_eq!(unreadable.window, None);
    assert_eq!(unreadable.state, ContextWindowCacheState::Unreadable);
}

#[test]
fn context_provenance_names_source_cache_age_ttl_and_discovery_state() {
    let cli = ContextWindowProvenance::pinned(ContextWindowSource::Cli);
    assert_eq!(cli.source.label(), "CLI --context-window");
    assert_eq!(cli.cache.label(), "not consulted");

    let cached = ContextWindowProvenance::cached(3_661);
    assert_eq!(cached.source.label(), "disk cache");
    assert_eq!(cached.cache.label(), "fresh (age 1h 1m; TTL 1d)");

    let pending = ContextWindowProvenance::defaulted(
        ContextWindowCacheState::Miss,
        ContextWindowDiscoveryState::Pending,
    );
    assert_eq!(pending.source.label(), "conservative default");
    assert!(pending.discovery.label().contains("in progress"));
    assert!(pending.discovery.label().contains("next run"));
}

#[test]
fn context_cache_key_does_not_embed_endpoint_credentials() {
    let key = context_window_cache_key(
        "https://user:secret@example.test/token/path",
        Some("/v1/models"),
        "model-a",
    );
    assert!(key.starts_with("v2-fnv128-"));
    assert!(!key.contains("secret"));
    assert!(!key.contains("token"));
    assert_ne!(
        key,
        context_window_cache_key(
            "https://user:secret@example.test/token/path",
            Some("/different/models"),
            "model-a"
        ),
        "models_path is part of catalog identity"
    );
}

#[test]
fn context_cache_key_has_a_stable_versioned_vector() {
    assert_eq!(
        context_window_cache_key("https://example.test", Some("/v1/models"), "model-a"),
        "v2-fnv128-cade2cf31e9e82068d9af7d4d5f7be8a"
    );
}
