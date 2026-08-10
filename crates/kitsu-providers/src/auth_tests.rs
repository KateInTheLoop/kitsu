//! Tests for the auth layer (design §3). TESTER-owned; no production logic here.
//!
//! Env is process-global, so every test that touches env vars is `#[serial]` and
//! restores prior state via a scoped guard. Expiry timestamps are built relative
//! to `chrono::Utc::now()` (tests may call it; scripts may not) so the suite never
//! depends on a hardcoded wall-clock date.

use crate::auth::{AuthProvider, EnvBearerAuth, NoAuth, StaticAuth};
use crate::error::{AuthError, ExpiryStatus};
use chrono::{Duration, Utc};
use serial_test::serial;
use std::env;

/// Scoped env guard: sets `key=value` (or removes it when `value` is None) for the
/// lifetime of the guard, restoring the previous value on drop. Keeps the global
/// process env clean between serialized tests.
struct EnvGuard {
    key: String,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let prev = env::var(key).ok();
        env::set_var(key, value);
        Self {
            key: key.to_string(),
            prev,
        }
    }

    fn unset(key: &str) -> Self {
        let prev = env::var(key).ok();
        env::remove_var(key);
        Self {
            key: key.to_string(),
            prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => env::set_var(&self.key, v),
            None => env::remove_var(&self.key),
        }
    }
}

const TEST_ENV: &str = "KITSU_TEST_AUTH_TOKEN";
const TEST_EXPIRES_ENV: &str = "KITSU_TEST_TOKEN_EXPIRES";

fn auth_for_test() -> EnvBearerAuth {
    EnvBearerAuth::new(
        TEST_ENV,
        Some(TEST_EXPIRES_ENV.to_string()),
        Some("run `refresh-token` to refresh the gateway JWT".to_string()),
    )
}

// --- header() ---------------------------------------------------------------

/// No-auth is a successful, explicit outcome — never an AuthError sentinel that
/// can also be produced by a misconfigured credential provider.
#[test]
fn test_no_auth_header_is_explicit_success() {
    assert!(
        NoAuth
            .header()
            .expect("NoAuth should be a successful outcome")
            .is_none(),
        "NoAuth must not masquerade as a missing-credential error"
    );
}

/// An omitted static token is a configuration/auth failure, not an implicit
/// request to send no credentials.
#[test]
fn test_empty_static_token_is_not_treated_as_no_auth() {
    let err = StaticAuth::new("")
        .header()
        .expect_err("an empty static token must fail");
    assert!(
        err.to_string().contains("static token"),
        "error should identify the missing static token: {err}"
    );
}

/// design §3: header() reads the token from the configured env var and returns
/// ("authorization", "Bearer <token>").
#[test]
#[serial]
fn test_header_reads_token_from_env() {
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    let auth = auth_for_test();

    let (name, value) = auth
        .header()
        .expect("header() should succeed when token is set")
        .expect("bearer auth should produce a header");

    assert_eq!(name.as_str(), "authorization");
    assert_eq!(value.to_str().unwrap(), "Bearer test-token");
}

/// design §3: the returned HeaderValue MUST be marked sensitive so reqwest/tracing
/// never logs it.
#[test]
#[serial]
fn test_header_value_is_sensitive() {
    let _g = EnvGuard::set(TEST_ENV, "test-token");
    let auth = auth_for_test();

    let (_name, value) = auth
        .header()
        .expect("header() should succeed when token is set")
        .expect("bearer auth should produce a header");
    assert!(
        value.is_sensitive(),
        "auth HeaderValue must be marked sensitive"
    );
}

/// design §3 step 2: missing env var => AuthError::Missing carrying the env name
/// and refresh hint.
#[test]
#[serial]
fn test_header_missing_env_is_missing_error() {
    let _g = EnvGuard::unset(TEST_ENV);
    let auth = auth_for_test();

    let err = auth
        .header()
        .expect_err("header() should fail when env is unset");
    match err {
        AuthError::Missing { env, hint } => {
            assert_eq!(env, TEST_ENV);
            let hint = hint.expect("refresh hint should be propagated into Missing");
            assert!(
                hint.contains("refresh-token"),
                "missing-token hint should mention `refresh-token`, got: {hint}"
            );
        }
        other => panic!("expected AuthError::Missing, got {other:?}"),
    }
}

/// design §3 step 2: an empty token is treated the same as missing.
#[test]
#[serial]
fn test_header_empty_env_is_missing_error() {
    let _g = EnvGuard::set(TEST_ENV, "");
    let auth = auth_for_test();

    let err = auth
        .header()
        .expect_err("header() should fail when env is empty");
    assert!(
        matches!(err, AuthError::Missing { .. }),
        "empty token must map to AuthError::Missing, got {err:?}"
    );
}

/// The refresh hint must surface in the Display of the Missing error (so the CLI
/// can print an actionable message), and the token value must never appear.
#[test]
#[serial]
fn test_missing_error_display_has_hint_not_token() {
    let _g = EnvGuard::unset(TEST_ENV);
    let auth = auth_for_test();

    let err = auth
        .header()
        .expect_err("header() should fail when env is unset");
    let display = err.to_string();
    assert!(
        display.contains("refresh-token"),
        "Display should include the refresh hint: {display}"
    );
}

// --- expiry_status() --------------------------------------------------------

/// design §3: ANTHROPIC_TOKEN_EXPIRES in the FUTURE => Valid.
#[test]
#[serial]
fn test_expiry_future_is_valid() {
    let future = (Utc::now() + Duration::hours(6)).to_rfc3339();
    let _ge = EnvGuard::set(TEST_EXPIRES_ENV, &future);
    let auth = auth_for_test();

    assert!(
        matches!(auth.expiry_status(), ExpiryStatus::Valid { .. }),
        "a future expiry timestamp must be Valid"
    );
}

/// design §3: ANTHROPIC_TOKEN_EXPIRES in the PAST => Expired.
#[test]
#[serial]
fn test_expiry_past_is_expired() {
    let past = (Utc::now() - Duration::hours(6)).to_rfc3339();
    let _ge = EnvGuard::set(TEST_EXPIRES_ENV, &past);
    let auth = auth_for_test();

    assert!(
        matches!(auth.expiry_status(), ExpiryStatus::Expired { .. }),
        "a past expiry timestamp must be Expired"
    );
}

/// design §3: unset expires env => Unknown (never fatal).
#[test]
#[serial]
fn test_expiry_unset_is_unknown() {
    let _ge = EnvGuard::unset(TEST_EXPIRES_ENV);
    let auth = auth_for_test();

    assert_eq!(
        auth.expiry_status(),
        ExpiryStatus::Unknown,
        "an unset expiry env must be Unknown"
    );
}

/// design §3: an unparseable timestamp => Unknown (do not error; env-var-only,
/// no JWT decoding).
#[test]
#[serial]
fn test_expiry_unparseable_is_unknown() {
    let _ge = EnvGuard::set(TEST_EXPIRES_ENV, "not-a-timestamp");
    let auth = auth_for_test();

    assert_eq!(
        auth.expiry_status(),
        ExpiryStatus::Unknown,
        "a garbage expiry timestamp must degrade to Unknown"
    );
}
