//! Integration tests for the `kitsu` binary (design §6 MVP, §7 CLI test plan).
//! TESTER-owned. These will FAIL until the Developer wires the CLI (config
//! loading, `--gateway`, the `ask` command, and the agent loop) — that is correct
//! TDD: the contract is asserted first.
//!
//! CONFIG PATH ASSUMPTION: design §6 references `~/.kitsu/config.toml` but does not
//! pin a config-path flag. These tests therefore point the process at a temp HOME
//! containing `.kitsu/config.toml`. If the Developer instead supports a `--config`
//! flag or `KITSU_CONFIG` env var, switch `configure_cmd` to use it; the temp file
//! contents stay identical. Documented here so the Developer knows the expectation.
//!
//! Tokens: every test uses the dummy value "test-token"; the real
//! ANTHROPIC_AUTH_TOKEN is never read. Env-touching tests are `#[serial]`.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

const CHAT_PATH: &str = "/v1/chat/completions";
const TOKEN: &str = "test-token";

/// Write `~/.kitsu/config.toml` under `home` with a single gateway "example" whose
/// base_url points at `base_url` (a wiremock URI or a dummy unreachable URL).
fn write_config(home: &Path, base_url: &str) {
    let kitsu_dir = home.join(".kitsu");
    fs::create_dir_all(&kitsu_dir).expect("create ~/.kitsu");
    let config = format!(
        r#"default_gateway = "example"

[gateways.example]
flavor       = "openai"
base_url     = "{base_url}"
chat_path    = "{CHAT_PATH}"
models_path  = ""
model        = "example-model-v1"
auth         = {{ method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN", expires_env = "ANTHROPIC_TOKEN_EXPIRES", refresh_hint = "run `refresh-token` to refresh the gateway JWT" }}
"#
    );
    fs::write(kitsu_dir.join("config.toml"), config).expect("write config.toml");
}

/// A `kitsu` Command with HOME pointed at the temp dir and the token env cleared
/// to a known state. Callers set ANTHROPIC_AUTH_TOKEN as needed.
fn base_cmd(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("kitsu").expect("kitsu binary should build");
    cmd.env("HOME", home);
    // Ensure no ambient expiry interferes.
    cmd.env_remove("ANTHROPIC_TOKEN_EXPIRES");
    // Don't auto-load the repo's own llms.txt into test system prompts.
    cmd.env("KITSU_NO_PROJECT_CONTEXT", "1");
    cmd
}

// --- missing token ----------------------------------------------------------

/// design §6 acceptance: missing token → exit 1 with the `refresh-token` hint.
/// base_url is a dummy unreachable address; the token check must fire before any
/// network attempt, so no request is made.
#[test]
#[serial]
fn test_ask_missing_token() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "https://gateway.invalid");

    let mut cmd = base_cmd(home.path());
    cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.args(["ask", "--gateway", "example", "hi"]);

    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("refresh-token"));
}

/// A positional prompt after `--` is never reinterpreted by the outer error
/// reporter as a request for JSONL framing.
#[test]
#[serial]
fn test_jsonl_word_after_double_dash_remains_plain_prompt_text() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "https://gateway.invalid");

    let mut cmd = base_cmd(home.path());
    cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.args(["ask", "--", "--jsonl"]);
    cmd.assert()
        .failure()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("refresh-token"))
        .stderr(predicate::str::contains("\"schema\":\"kitsu.chat.v1\"").not());
}

/// Scripted chat classifies a locally missing credential the same way as
/// `ask`: user/config error (1), not provider outage (2).
#[test]
#[serial]
fn test_script_missing_token_exits_one() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "https://gateway.invalid");
    let script = home.path().join("missing-auth.txt");
    fs::write(&script, "hello\n").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.args([
        "chat",
        "--gateway",
        "example",
        "--script",
        &script.to_string_lossy(),
    ]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("refresh-token"))
        .stderr(predicate::str::contains("script summary: 1 failure"));
}

/// Auth methods are a closed config vocabulary. A typo must fail before any
/// provider/network work instead of silently becoming bearer-env authentication.
#[test]
fn test_unknown_auth_method_fails_fast() {
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).expect("create ~/.kitsu");
    fs::write(
        kitsu_dir.join("config.toml"),
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = ""
model = "test-model"
context_window = 8000
auth = { method = "bearer_env_typo", env = "KITSU_TEST_UNKNOWN_AUTH_TOKEN" }
"#,
    )
    .expect("write config.toml");

    let mut cmd = base_cmd(home.path());
    cmd.env_remove("KITSU_TEST_UNKNOWN_AUTH_TOKEN");
    cmd.args(["ask", "--gateway", "example", "hi"]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "unsupported auth method 'bearer_env_typo'",
        ));
}

/// `method = "static"` requires a non-empty token. Missing it must be reported
/// as a config error rather than being reinterpreted as `method = "none"`.
#[test]
fn test_static_auth_without_token_fails_fast() {
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).expect("create ~/.kitsu");
    fs::write(
        kitsu_dir.join("config.toml"),
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = ""
model = "test-model"
context_window = 8000
auth = { method = "static" }
"#,
    )
    .expect("write config.toml");

    let mut cmd = base_cmd(home.path());
    cmd.args(["ask", "--gateway", "example", "hi"]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "static auth requires a non-empty token",
        ));
}

/// A stateless automation invocation must not require a usable HOME merely to
/// resolve where interactive history would live.
#[test]
fn test_empty_script_does_not_require_writable_home() {
    let root = TempDir::new().expect("temp root");
    let blocked_home = root.path().join("home-is-a-file");
    fs::write(&blocked_home, "not a directory").expect("write blocked HOME sentinel");
    let config = root.path().join("kitsu.toml");
    fs::write(
        &config,
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = ""
model = "test-model"
context_window = 8000
auth = { method = "none" }
"#,
    )
    .expect("write config");
    let script = root.path().join("empty.script");
    fs::write(&script, "").expect("write empty script");

    let mut cmd = Command::cargo_bin("kitsu").expect("kitsu binary should build");
    cmd.env("HOME", &blocked_home)
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .args([
            "--config",
            &config.to_string_lossy(),
            "chat",
            "--script",
            &script.to_string_lossy(),
        ]);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains("cannot create runtime state").not());
}

// --- happy path against wiremock --------------------------------------------

/// design §6 acceptance: with a valid token and a mock gateway, `kitsu ask` exits
/// 0 and prints the model's text.
///
/// Uses a wiremock server inside a tokio runtime; the binary is spawned with
/// `tokio::task::spawn_blocking` so the async server keeps serving while the
/// synchronous assert_cmd process runs.
#[tokio::test]
#[serial]
async fn test_ask_against_mock() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // SSE completion: one text fragment + stop.
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"hello from mistral"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &base_url);
    let home_path = home.path().to_path_buf();

    // Run the blocking assert_cmd process off the async runtime so wiremock keeps
    // serving requests.
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["ask", "--gateway", "example", "hello"]);

        cmd.assert()
            .success()
            .stdout(predicate::str::contains("hello from mistral"))
            // the token must never be echoed to stdout
            .stdout(predicate::str::contains(TOKEN).not());
    })
    .await
    .expect("spawned kitsu process should join");
}

#[tokio::test]
#[serial]
async fn test_jsonl_ask_is_versioned_framed_and_parseable() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let completion = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "json answer"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 7, "completion_tokens": 2, "total_tokens": 9}
    });
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(completion),
        )
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["--jsonl", "ask", "hello"]);
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let events: Vec<serde_json::Value> = stdout
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect();
        assert!(events.len() >= 6, "events: {events:#?}");
        assert!(events
            .iter()
            .all(|event| event["schema"] == "kitsu.chat.v1"));
        assert_eq!(events.first().unwrap()["type"], "run_started");
        assert!(events
            .iter()
            .any(|event| event["type"] == "provider_request"));
        assert!(events
            .iter()
            .any(|event| { event["type"] == "usage" && event["total_tokens"] == 9 }));
        assert!(events
            .iter()
            .any(|event| { event["type"] == "assistant" && event["text"] == "json answer" }));
        assert_eq!(events.last().unwrap()["type"], "final_summary");
        assert_eq!(events.last().unwrap()["status"], "ok");
        assert!(!stdout.lines().any(|line| !line.starts_with('{')));
    })
    .await
    .unwrap();
}

#[tokio::test]
#[serial]
async fn test_jsonl_agent_provider_failures_are_typed_for_ask_and_piped_chat() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(503).set_body_string("provider unavailable"))
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        for piped in [false, true] {
            let mut cmd = base_cmd(&home_path);
            cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
            if piped {
                cmd.args(["--jsonl", "chat", "--no-session"])
                    .write_stdin("hello\n");
            } else {
                cmd.args(["--jsonl", "ask", "hello"]);
            }
            let output = cmd.output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let error = events
                .iter()
                .find(|event| event["type"] == "error")
                .expect("typed error event");
            assert_eq!(error["kind"], "provider", "piped={piped}");
            assert_eq!(error["exit_code"], 2, "piped={piped}");
        }
    })
    .await
    .unwrap();
}

#[test]
#[serial]
fn test_jsonl_preflight_error_keeps_stdout_machine_only() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "https://gateway.invalid");
    let mut cmd = base_cmd(home.path());
    cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.args(["--jsonl", "ask", "hello"]);
    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds: Vec<_> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "run_started",
            "turn_started",
            "provider_request",
            "error",
            "turn_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[3]["exit_code"], 1);
    assert_eq!(events[5]["user_failures"], 1);
}

#[test]
#[serial]
fn test_jsonl_parse_error_is_framed_and_has_a_final_summary() {
    let home = TempDir::new().unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.args(["--jsonl", "--max-rounds", "not-a-number", "ask", "hello"]);
    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stderr.is_empty(),
        "JSONL parse diagnostics leaked to stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    assert_eq!(events.len(), 2, "events: {events:#?}");
    assert_eq!(events[0]["type"], "error");
    assert_eq!(events[0]["exit_code"], 1);
    assert!(events[0]["message"]
        .as_str()
        .is_some_and(|message| message.contains("invalid value")));
    assert_eq!(events[1]["type"], "final_summary");
    assert_eq!(events[1]["status"], "error");
    assert_eq!(events[1]["user_failures"], 1);
}

#[test]
#[serial]
fn test_jsonl_rejects_human_help_and_version_without_raw_stdout() {
    let home = TempDir::new().unwrap();
    for flag in ["--help", "--version"] {
        let mut cmd = base_cmd(home.path());
        cmd.args(["--jsonl", flag]);
        let output = cmd.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "flag: {flag}");
        assert!(output.stderr.is_empty(), "flag: {flag}");
        let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect();
        assert_eq!(events.len(), 2, "flag: {flag}; events: {events:#?}");
        assert_eq!(events[0]["type"], "error", "flag: {flag}");
        assert_eq!(events[0]["exit_code"], 1, "flag: {flag}");
        assert_eq!(events[1]["type"], "final_summary", "flag: {flag}");
    }
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jsonl_sigint_emits_cancelled_turn_and_summary() {
    use std::process::Stdio;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_string("data: [DONE]\n\n"),
        )
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("kitsu"));
    cmd.env("HOME", home.path())
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["--jsonl", "ask", "wait for cancellation"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().unwrap();
    let pid = child.id();

    let mut received = false;
    for _ in 0..100 {
        if !server.received_requests().await.unwrap().is_empty() {
            received = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(received, "child never reached the hanging provider request");
    // SAFETY: `pid` belongs to the live child created immediately above.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) }, 0);

    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert!(
        output.stderr.is_empty(),
        "JSONL cancellation leaked human stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    let kinds: Vec<_> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "run_started",
            "turn_started",
            "provider_request",
            "error",
            "turn_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[3]["kind"], "cancelled");
    assert_eq!(events[3]["exit_code"], 130);
    assert_eq!(events[4]["status"], "cancelled");
    assert_eq!(events[5]["status"], "cancelled");
    assert_eq!(events[5]["cancelled_failures"], 1);
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jsonl_piped_stdin_sigint_before_eof_emits_cancelled_summary() {
    use std::process::Stdio;

    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = ""
model = "example-model"
context_window = 8192
auth = { method = "none" }
"#,
    )
    .unwrap();
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("kitsu"));
    cmd.env("HOME", home.path())
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .args(["--jsonl", "chat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let held_stdin = child.stdin.take().expect("hold the pipe open without EOF");
    let pid = child.id();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // SAFETY: `pid` belongs to the live child created immediately above.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) }, 0);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            drop(held_stdin);
            let _ = child.wait();
            panic!("piped stdin cancellation did not exit while stdin remained open");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(status.code(), Some(130));
    drop(held_stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.stderr.is_empty());
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["run_started", "error", "final_summary"]
    );
    assert_eq!(events[1]["kind"], "cancelled");
    assert_eq!(events[2]["status"], "cancelled");
    assert_eq!(events[2]["cancelled_failures"], 1);
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jsonl_script_fifo_read_sigint_exits_without_waiting_for_writer() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::process::Stdio;

    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = ""
model = "example-model"
context_window = 8192
auth = { method = "none" }
"#,
    )
    .unwrap();
    let fifo = home.path().join("blocked.script");
    let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `fifo_c` is a valid NUL-terminated path and points to writable
    // temporary storage owned by this test.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("kitsu"));
    cmd.env("HOME", home.path())
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .args(["--jsonl", "chat", "--script", &fifo.to_string_lossy()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let pid = child.id();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // SAFETY: `pid` belongs to the live child created immediately above.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) }, 0);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("script FIFO cancellation waited for a writer");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(status.code(), Some(130));
    let output = child.wait_with_output().unwrap();
    assert!(output.stderr.is_empty());
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["type"], "run_started");
    assert_eq!(events[1]["kind"], "cancelled");
    assert_eq!(events[2]["type"], "final_summary");
    assert_eq!(events[2]["status"], "cancelled");
    assert_eq!(events[2]["cancelled_failures"], 1);
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jsonl_script_slash_turn_cancellation_closes_command_frame() {
    use std::process::Stdio;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_string("data: [DONE]\n\n"),
        )
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let repo = TempDir::new().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap();
    fs::write(repo.path().join("untracked.rs"), "fn changed() {}\n").unwrap();
    let script = home.path().join("review.txt");
    fs::write(&script, "/review\n").unwrap();

    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("kitsu"));
    cmd.env("HOME", home.path())
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--jsonl",
            "chat",
            "--allow-path",
            &repo.path().to_string_lossy(),
            "--script",
            &script.to_string_lossy(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().unwrap();
    let pid = child.id();

    let mut received = false;
    for _ in 0..100 {
        if !server.received_requests().await.unwrap().is_empty() {
            received = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(received, "script never reached the hanging review request");
    // SAFETY: `pid` belongs to the live child created immediately above.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) }, 0);
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(130));
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    let kinds: Vec<_> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "run_started",
            "command_started",
            "turn_started",
            "provider_request",
            "error",
            "turn_finished",
            "command_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[4]["kind"], "cancelled");
    assert_eq!(events[5]["status"], "cancelled");
    assert_eq!(events[6]["status"], "cancelled");
    assert_eq!(events[7]["status"], "cancelled");
}

#[tokio::test]
#[serial]
async fn test_models_command_does_not_require_a_configured_model() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "model-b"}, {"id": "model-a"}]
        })))
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
models_path = "/v1/models"
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.args(["models"]);
        cmd.assert().success().stdout("model-a\nmodel-b\n");
    })
    .await
    .unwrap();
}

#[tokio::test]
#[serial]
async fn test_models_command_sanitizes_provider_control_sequences() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "safe\u{1b}]8;;https://evil.invalid\u{7}click\u{1b}]8;;\u{7}"}]
        })))
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
models_path = "/v1/models"
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.args(["models"]);
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("safe"))
            .stdout(predicate::str::contains("\u{1b}").not())
            .stdout(predicate::str::contains("https://evil.invalid").not());
    })
    .await
    .unwrap();
}

#[tokio::test]
#[serial]
async fn test_jsonl_top_level_models_provider_failure_is_typed() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(503).set_body_string("catalog unavailable"))
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
models_path = "/v1/models"
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.args(["--jsonl", "models"]);
        let output = cmd.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let error = events
            .iter()
            .find(|event| event["type"] == "error")
            .expect("typed error event");
        assert_eq!(error["kind"], "provider");
        assert_eq!(error["exit_code"], 2);
        assert_eq!(events.last().unwrap()["provider_failures"], 1);
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jsonl_top_level_models_sigint_has_cancelled_summary() {
    use std::process::Stdio;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_json(serde_json::json!({"data": []})),
        )
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
models_path = "/v1/models"
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("kitsu"));
    cmd.env("HOME", home.path())
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .args(["--jsonl", "models"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().unwrap();
    let pid = child.id();
    for _ in 0..100 {
        if !server.received_requests().await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!server.received_requests().await.unwrap().is_empty());
    // SAFETY: `pid` belongs to the live child created immediately above.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) }, 0);
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert!(output.stderr.is_empty());
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 3, "events: {events:#?}");
    assert_eq!(events[0]["type"], "run_started");
    assert_eq!(events[1]["kind"], "cancelled");
    assert_eq!(events[1]["exit_code"], 130);
    assert_eq!(events[2]["type"], "final_summary");
    assert_eq!(events[2]["status"], "cancelled");
    assert_eq!(events[2]["cancelled_failures"], 1);
}

// --- chat: piped-stdin one-shot ---------------------------------------------

/// `echo "hi" | kitsu chat` (stdin not a TTY) runs one turn and exits 0,
/// printing the model text; the token is never echoed.
#[tokio::test]
#[serial]
async fn test_chat_piped_stdin_one_shot() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"hi from chat"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &base_url);
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["chat", "--gateway", "example"]);
        cmd.write_stdin("hello\n"); // piped → non-TTY → one-shot
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("hi from chat"))
            .stdout(predicate::str::contains(TOKEN).not());
    })
    .await
    .expect("spawned kitsu chat should join");
}

// --- chat: end-to-end tool round --------------------------------------------

/// With `--allow-path` set, the model can request `fs_read_file` and the tool
/// loop runs it: round 1 returns a tool_calls SSE, round 2 returns text. The
/// file content (read from the allowed temp dir) reaches the second request.
#[tokio::test]
#[serial]
async fn test_chat_tool_round_reads_file() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // A temp dir we will allow, containing a file with a known marker.
    let work = TempDir::new().expect("work dir");
    let file = work.path().join("note.txt");
    fs::write(&file, "SECRET_MARKER_42").expect("write note");
    let file_str = file.to_string_lossy().to_string();

    // Round 1: model asks to read the file (only matches the FIRST request,
    // which has no tool result yet).
    let tool_args = format!(r#"{{\"path\":\"{}\"}}"#, file_str.replace('\\', "\\\\"));
    let sse_round1 = [
        format!(
            r#"data: {{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":"call_1","type":"function","function":{{"name":"fs_read_file","arguments":"{tool_args}"}}}}]}},"finish_reason":null}}]}}"#
        ),
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#.to_string(),
        "data: [DONE]".to_string(),
        "".to_string(),
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_round1),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Round 2: any later request (which now carries the tool result) → final text.
    let sse_round2 = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"the file says SECRET_MARKER_42"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_round2),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &base_url);
    let home_path = home.path().to_path_buf();
    let allow = work.path().to_string_lossy().to_string();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["chat", "--gateway", "example", "--allow-path", &allow]);
        cmd.write_stdin("read the note\n");
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("SECRET_MARKER_42"));
    })
    .await
    .expect("spawned kitsu chat tool round should join");
    // keep `work` alive until the process has run
    drop(work);
}

/// `--allow-write <dir>` alone (no `--allow-path`) must still let the model
/// read a file back from that same directory — editing an existing file is
/// read-then-write, and a write-only grant that can never be read back is a
/// trap nobody asks for. Regression test for a dogfooding finding where a
/// scripted edit turn failed at the read step with PolicyDenied even though
/// the user had explicitly granted write access to the exact same directory.
#[tokio::test]
#[serial]
async fn test_allow_write_implies_read_of_same_root() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    let work = TempDir::new().expect("work dir");
    let file = work.path().join("note.txt");
    fs::write(&file, "SECRET_MARKER_43").expect("write note");
    let file_str = file.to_string_lossy().to_string();

    let tool_args = format!(r#"{{\"path\":\"{}\"}}"#, file_str.replace('\\', "\\\\"));
    let sse_round1 = [
        format!(
            r#"data: {{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":"call_1","type":"function","function":{{"name":"fs_read_file","arguments":"{tool_args}"}}}}]}},"finish_reason":null}}]}}"#
        ),
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#.to_string(),
        "data: [DONE]".to_string(),
        "".to_string(),
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_round1),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let sse_round2 = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"the file says SECRET_MARKER_43"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_round2),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &base_url);
    let home_path = home.path().to_path_buf();
    let allow = work.path().to_string_lossy().to_string();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        // --allow-write only — no --allow-path.
        cmd.args(["chat", "--gateway", "example", "--allow-write", &allow]);
        cmd.write_stdin("read the note\n");
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("SECRET_MARKER_43"));
    })
    .await
    .expect("spawned kitsu chat tool round should join");
    drop(work);
}

// --- --max-rounds flag + usage surfacing ------------------------------------

/// The global `--max-rounds` flag parses and a normal turn completes under it,
/// and the gateway-reported usage is surfaced on stderr as the `kitsu-usage:`
/// line (cost reporting). The usage rides the final
/// SSE chunk via stream_options.include_usage.
#[tokio::test]
#[serial]
async fn test_max_rounds_flag_and_usage_line() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &base_url);
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        // --max-rounds is global; placed before the subcommand.
        cmd.args(["--max-rounds", "40", "ask", "--gateway", "example", "hello"]);
        cmd.assert()
            .success()
            // Streaming owns exactly one trailing newline, even when telemetry
            // is emitted after the response.
            .stdout(predicate::eq("answer\n"))
            // usage surfaced to stderr, never stdout
            .stderr(predicate::str::contains("kitsu-usage:"))
            .stderr(predicate::str::contains("\"total_tokens\":14"))
            .stdout(predicate::str::contains("kitsu-usage:").not());
    })
    .await
    .expect("spawned kitsu --max-rounds process should join");
}

/// The global `--timeout-secs` flag parses and a normal turn completes under it
/// (robustness fix). We only assert the flag is accepted and the turn runs;
/// exercising an actual timeout would require a deliberately-slow mock.
#[tokio::test]
#[serial]
async fn test_timeout_secs_flag_parses() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &base_url);
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["--timeout-secs", "300", "ask", "--gateway", "example", "hi"]);
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("ok"));
    })
    .await
    .expect("spawned kitsu --timeout-secs process should join");
}

// --- copy-paste: multi-line block is one turn -------------------------------

/// A `<<<` … `>>>` block in a `--script` must be sent as ONE user turn (the
/// scripted equivalent of a bracketed multi-line paste), preserving newlines.
#[tokio::test]
#[serial]
async fn test_script_multiline_block_is_one_turn() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &base_url);

    // A pasted code block spanning multiple lines, wrapped in <<< / >>>.
    let script = "<<<\ndef add(a, b):\n    return a + b\n>>>\n";
    let script_path = home.path().join("scn.txt");
    fs::write(&script_path, script).expect("write script");
    let script_str = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["chat", "--gateway", "example", "--script", &script_str]);
        cmd.assert().success();
    })
    .await
    .expect("spawned kitsu chat --script process should join");

    // Exactly one request → the block was ONE turn, not three separate lines.
    let reqs = server.received_requests().await.expect("recorded requests");
    assert_eq!(reqs.len(), 1, "multi-line block must be a single turn");
    let body = String::from_utf8_lossy(&reqs[0].body);
    assert!(
        body.contains("def add(a, b):"),
        "body missing first line: {body}"
    );
    assert!(
        body.contains("return a + b"),
        "body missing second line: {body}"
    );
    // Both lines must live in the SAME user message (joined by an escaped \n).
    assert!(
        body.contains("def add(a, b):\\n    return a + b"),
        "the block should be one user message with an embedded newline: {body}"
    );
}

// --- ISSUE-004: friendly error when the gateway rejects the model -----------

/// A gateway 404 (model not found) must render an actionable hint naming the
/// model and pointing at /model, not the raw HTTP JSON.
#[tokio::test]
#[serial]
async fn test_invalid_model_friendly_error() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(404).set_body_string(
            r#"{"error":{"message":"model 'tiny-fast-thing' not found","type":"api_error"}}"#,
        ))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &base_url);
    let script = "hello\n";
    let script_path = home.path().join("scn.txt");
    fs::write(&script_path, script).expect("write script");
    let script_str = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args([
            "--gateway",
            "example",
            "--model",
            "tiny-fast-thing",
            "chat",
            "--script",
            &script_str,
        ]);
        cmd.assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains("run /model"))
            .stderr(predicate::str::contains("tiny-fast-thing"))
            .stderr(predicate::str::contains("script summary: 1 failure"));
    })
    .await
    .expect("spawned kitsu process should join");
}

/// Automation is strict by default, but callers can explicitly accept partial
/// success while migrating legacy scripts.
#[tokio::test]
#[serial]
async fn test_script_allow_turn_errors_preserves_zero_exit() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(503).set_body_string("temporarily unavailable"))
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script_path = home.path().join("allow-errors.txt");
    fs::write(&script_path, "will fail\n").unwrap();
    let script = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &script,
            "--allow-turn-errors",
        ]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("script summary: 1 failure"));
    })
    .await
    .expect("join");
}

#[tokio::test]
#[serial]
async fn test_strict_script_rejects_length_truncated_turn() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"partial answer"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("truncated.txt");
    fs::write(&script, "long answer please\n").unwrap();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args([
            "chat",
            "--gateway",
            "example",
            "--script",
            &script.to_string_lossy(),
        ]);
        cmd.assert()
            .failure()
            .code(2)
            .stdout(predicate::str::contains("partial answer"))
            .stderr(predicate::str::contains(
                "strict scripted turn failed: model output was truncated",
            ))
            .stderr(predicate::str::contains("kitsu-context-rollback"));
    })
    .await
    .expect("join");
}

/// A failed scripted turn is removed from conversation state before the next
/// line is sent. This prevents a provider error from poisoning later context.
#[tokio::test]
#[serial]
async fn test_failed_script_turn_is_transactionally_rolled_back() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let success_sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"recovered"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |request: &wiremock::Request| {
            let body = String::from_utf8_lossy(&request.body);
            if body.contains("FIRST_FAILED_PROMPT") {
                ResponseTemplate::new(500).set_body_string("first turn failed")
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(success_sse.clone())
            }
        })
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script_path = home.path().join("transaction.txt");
    fs::write(
        &script_path,
        "FIRST_FAILED_PROMPT\nSECOND_RECOVERY_PROMPT\n",
    )
    .unwrap();
    let script = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &script,
            "--allow-turn-errors",
        ]);
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("recovered"))
            .stderr(predicate::str::contains("failed turn rolled back"));
    })
    .await
    .expect("join");

    let requests = server.received_requests().await.unwrap();
    let recovery = requests
        .iter()
        .find(|request| String::from_utf8_lossy(&request.body).contains("SECOND_RECOVERY_PROMPT"))
        .expect("recovery request");
    let body = String::from_utf8_lossy(&recovery.body);
    assert!(
        !body.contains("FIRST_FAILED_PROMPT"),
        "failed prompt leaked: {body}"
    );
}

#[test]
fn test_unterminated_script_block_is_line_aware_and_nonzero() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home.path().join("broken.txt");
    fs::write(&script_path, "# comment\n<<<\nmissing close\n").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    cmd.args([
        "--gateway",
        "example",
        "chat",
        "--script",
        &script_path.to_string_lossy(),
    ]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("broken.txt:2"))
        .stderr(predicate::str::contains("missing >>>"));
}

#[test]
fn test_explicit_missing_config_names_the_requested_path() {
    let home = TempDir::new().expect("temp home");
    let missing = home.path().join("does-not-exist.toml");
    let mut cmd = base_cmd(home.path());
    cmd.args(["--config", &missing.to_string_lossy(), "ask", "hello"]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(missing.to_string_lossy().as_ref()))
        .stderr(predicate::str::contains("does not exist"));
}

#[test]
fn test_default_missing_config_has_a_quick_start() {
    let home = TempDir::new().expect("temp home");
    let mut cmd = base_cmd(home.path());
    cmd.args(["chat"]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("no config found"))
        .stderr(predicate::str::contains("kitsu --config ./kitsu.toml chat"));
}

/// `/diff` is sourced from Git and the filesystem, including untracked files;
/// it requires no model call and is visibly labelled as verified evidence.
#[test]
fn test_diff_command_reports_verified_tracked_and_untracked_changes() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let work = TempDir::new().expect("work dir");
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(work.path())
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "--quiet"]);
    fs::write(work.path().join("tracked.txt"), "before\n").unwrap();
    git(&["add", "tracked.txt"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--quiet",
        "-m",
        "base",
    ]);
    fs::write(work.path().join("tracked.txt"), "after\n").unwrap();
    fs::write(work.path().join("untracked.txt"), "new evidence\n").unwrap();
    let script_path = work.path().join("diff.txt");
    fs::write(&script_path, "/diff\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    cmd.current_dir(work.path());
    cmd.args([
        "--gateway",
        "example",
        "chat",
        "--allow-path",
        &work.path().to_string_lossy(),
        "--script",
        &script_path.to_string_lossy(),
    ]);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains("command> /diff"))
        .stderr(predicate::str::contains("verified workspace diff"))
        .stderr(predicate::str::contains("-before"))
        .stderr(predicate::str::contains("+after"))
        .stderr(predicate::str::contains("new file (untracked)"))
        .stderr(predicate::str::contains("+new evidence"));
}

#[test]
fn test_diff_from_subdirectory_requires_root_permission_and_resolves_filter() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let repo = TempDir::new().unwrap();
    let package = repo.path().join("crates/widget");
    fs::create_dir_all(package.join("src")).unwrap();
    let git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "--quiet"]);
    fs::write(package.join("src/lib.rs"), "before\n").unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "--quiet",
        "-m",
        "base",
    ]);
    fs::write(package.join("src/lib.rs"), "after\n").unwrap();
    let script = package.join("diff-script.txt");
    fs::write(&script, "/diff src/lib.rs\n/quit\n").unwrap();

    let mut denied = base_cmd(home.path());
    denied.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    denied.current_dir(&package);
    denied.args([
        "--gateway",
        "example",
        "chat",
        "--allow-path",
        &package.to_string_lossy(),
        "--script",
        &script.to_string_lossy(),
    ]);
    denied
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("read permission required"));

    let mut allowed = base_cmd(home.path());
    allowed.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    allowed.current_dir(&package);
    allowed.args([
        "--gateway",
        "example",
        "chat",
        "--allow-path",
        &repo.path().to_string_lossy(),
        "--script",
        &script.to_string_lossy(),
    ]);
    allowed
        .assert()
        .success()
        .stderr(predicate::str::contains("crates/widget/src/lib.rs"))
        .stderr(predicate::str::contains("+after"));
}

#[tokio::test]
#[serial]
async fn test_review_turn_does_not_offer_or_execute_write_tools() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let repo = TempDir::new().unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    fs::write(repo.path().join("change.txt"), "review me\n").unwrap();
    let attempted = repo.path().join("review-must-not-write.txt");
    let tool_args = format!(
        r#"{{\"path\":\"{}\",\"content\":\"MUTATED\"}}"#,
        attempted.to_string_lossy().replace('\\', "\\\\")
    );
    let malicious = [
        format!(
            r#"data: {{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":"call_write","type":"function","function":{{"name":"fs_write_file","arguments":"{tool_args}"}}}}]}},"finish_reason":null}}]}}"#
        ),
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#.to_string(),
        "data: [DONE]".to_string(),
        "".to_string(),
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(malicious),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let final_answer = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"review complete"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(final_answer),
        )
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = repo.path().join("review.txt");
    fs::write(&script, "/review\n").unwrap();
    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let script_path = script.clone();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.current_dir(&repo_path);
        cmd.args([
            "--gateway",
            "example",
            "chat",
            "--allow-path",
            &repo_path.to_string_lossy(),
            "--allow-write",
            &repo_path.to_string_lossy(),
            "--script",
            &script_path.to_string_lossy(),
        ]);
        cmd.assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains("tool call(s) failed"));
    })
    .await
    .unwrap();

    assert!(!attempted.exists(), "review executed a write-capable tool");
    let requests = server.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    for request in requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(
            body.get("tools")
                .map(serde_json::Value::is_null)
                .unwrap_or(true),
            "review request advertised tools: {body}"
        );
    }
}

#[test]
fn test_project_context_source_is_disclosed_and_can_be_disabled() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let work = TempDir::new().expect("work dir");
    fs::create_dir_all(work.path().join(".git")).unwrap();
    let context_path = work.path().join("AGENTS.md");
    fs::write(&context_path, "PROJECT_CONTEXT_MARKER\n").unwrap();
    let script_path = work.path().join("status.txt");
    fs::write(&script_path, "/status\n/quit\n").unwrap();

    let mut enabled = base_cmd(home.path());
    enabled.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    enabled.env_remove("KITSU_NO_PROJECT_CONTEXT");
    enabled.current_dir(work.path());
    enabled.args([
        "--gateway",
        "example",
        "chat",
        "--script",
        &script_path.to_string_lossy(),
    ]);
    enabled
        .assert()
        .success()
        .stderr(predicate::str::contains("loaded project context from"))
        .stderr(predicate::str::contains(
            context_path.to_string_lossy().as_ref(),
        ));

    let mut disabled = base_cmd(home.path());
    disabled.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    disabled.env_remove("KITSU_NO_PROJECT_CONTEXT");
    disabled.current_dir(work.path());
    disabled.args([
        "--gateway",
        "example",
        "--no-project-context",
        "chat",
        "--script",
        &script_path.to_string_lossy(),
    ]);
    disabled
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "project ctx  disabled / not found",
        ))
        .stderr(predicate::str::contains("loaded project context from").not());
}

#[test]
fn test_unknown_script_command_is_echoed_suggested_and_nonzero() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home.path().join("typo.txt");
    fs::write(&script_path, "/statsu\n").unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    cmd.args([
        "--gateway",
        "example",
        "chat",
        "--script",
        &script_path.to_string_lossy(),
    ]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("command> /statsu"))
        .stderr(predicate::str::contains("did you mean /status"))
        .stderr(predicate::str::contains("typo.txt:1"));
}

#[test]
fn test_script_transcript_echo_strips_terminal_control_sequences() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home.path().join("control.txt");
    fs::write(
        &script_path,
        "/unknown\u{1b}]8;;https://evil.invalid\u{7}click\u{1b}]8;;\u{7}\n",
    )
    .unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "chat",
        "--script",
        &script_path.to_string_lossy(),
    ]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("command> /unknown"))
        .stderr(predicate::str::contains("\u{1b}").not())
        .stderr(predicate::str::contains("https://evil.invalid").not());
}

#[cfg(unix)]
#[test]
fn test_script_error_locations_strip_terminal_control_sequences() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home
        .path()
        .join("unsafe\u{1b}]0;evil.invalid\u{7}click.txt");
    fs::write(&script_path, "/retry\n/resume last\n").unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "chat",
        "--script",
        &script_path.to_string_lossy(),
    ]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("nothing to retry"))
        .stderr(predicate::str::contains(
            "session commands require interactive chat",
        ))
        .stderr(predicate::str::contains("\u{1b}").not())
        .stderr(predicate::str::contains("evil.invalid").not());
}

#[test]
fn test_skills_slash_sanitizes_repository_metadata() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let repo = TempDir::new().expect("temp repo");
    fs::create_dir_all(repo.path().join(".git")).unwrap();
    let skill = repo.path().join(".kitsu/skills/unsafe");
    fs::create_dir_all(&skill).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: safe\u{1b}]8;;https://evil.invalid\u{7}click\u{1b}]8;;\u{7}\ndescription: test\n---\nbody",
    )
    .unwrap();
    let script = repo.path().join("skills.txt");
    fs::write(&script, "/skills\n").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .env_remove("KITSU_NO_PROJECT_CONTEXT")
        .current_dir(repo.path())
        .args(["chat", "--script", &script.to_string_lossy()]);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains("skills:"))
        .stderr(predicate::str::contains("safe"))
        .stderr(predicate::str::contains("\u{1b}").not())
        .stderr(predicate::str::contains("https://evil.invalid").not());
}

#[test]
fn test_yes_does_not_enable_commands_but_explicit_command_flags_do() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home.path().join("tools.txt");
    fs::write(&script_path, "/tools\n/quit\n").unwrap();

    let mut autoapprove_only = base_cmd(home.path());
    autoapprove_only.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "--yes",
        "chat",
        "--script",
        &script_path.to_string_lossy(),
    ]);
    autoapprove_only
        .assert()
        .success()
        .stderr(predicate::str::contains("command_exec").not());

    let mut commands_enabled = base_cmd(home.path());
    commands_enabled.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "chat",
        "--enable-commands",
        "--script",
        &script_path.to_string_lossy(),
    ]);
    commands_enabled
        .assert()
        .success()
        .stderr(predicate::str::contains("command_exec"));

    let mut exact_pregrant = base_cmd(home.path());
    exact_pregrant.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "chat",
        "--allow-command",
        r#"["cargo","--version"]"#,
        "--script",
        &script_path.to_string_lossy(),
    ]);
    exact_pregrant
        .assert()
        .success()
        .stderr(predicate::str::contains("command_exec"));
}

// --- /models command --------------------------------------------------------

/// `/models` lists the models the gateway's models endpoint returns.
#[tokio::test]
#[serial]
async fn test_models_slash_command_lists_models() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // /v1/models catalog
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"object":"list","data":[{"id":"alpha-model"},{"id":"beta-model"}]}"#,
        ))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    // Config with a real models_path so list_models() hits the endpoint.
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{base_url}"
chat_path = "{CHAT_PATH}"
models_path = "/v1/models"
model = "alpha-model"
auth = {{ method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN" }}
"#
        ),
    )
    .unwrap();

    let script = "/models\n/quit\n";
    let script_path = home.path().join("scn.txt");
    fs::write(&script_path, script).unwrap();
    let script_str = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["--gateway", "example", "chat", "--script", &script_str]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("alpha-model"))
            .stderr(predicate::str::contains("beta-model"))
            .stderr(predicate::str::contains("2 model(s)"));
    })
    .await
    .expect("join");
}

/// User-invoked catalog discovery honors `--timeout-secs`; it is deliberately
/// isolated from the two-second advisory startup probe and its negative cache.
#[tokio::test]
#[serial]
async fn test_models_slash_command_uses_cli_timeout_not_advisory_timeout() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(2300))
                .set_body_json(serde_json::json!({"data": [{"id": "slow-model"}]})),
        )
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
chat_path = "{CHAT_PATH}"
models_path = "/v1/models"
model = "slow-model"
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let script = home.path().join("models-timeout.txt");
    fs::write(&script, "/models\n").unwrap();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.args([
            "--timeout-secs",
            "4",
            "chat",
            "--script",
            &script.to_string_lossy(),
        ]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("slow-model"));
    })
    .await
    .unwrap();
}

/// Provider failures inside slash commands retain provider classification in
/// strict scripts and carry enough JSONL detail to diagnose without stderr.
#[tokio::test]
#[serial]
async fn test_jsonl_script_models_failure_is_typed_as_provider() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(503).set_body_string("catalog unavailable"))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
chat_path = "{CHAT_PATH}"
models_path = "/v1/models"
model = "example-model"
context_window = 8192
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let script = home.path().join("models-failure.txt");
    fs::write(&script, "/models\n").unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().to_string();

    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.args(["--jsonl", "chat", "--script", &script_path]);
        cmd.output().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    let error = events
        .iter()
        .find(|event| event["type"] == "error")
        .expect("typed command error");
    assert_eq!(error["kind"], "provider");
    assert_eq!(error["exit_code"], 2);
    assert!(error["message"].as_str().unwrap().contains("HTTP 503"));
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["provider_failures"], 1);
    assert_eq!(summary["user_failures"], 0);
}

#[tokio::test]
#[serial]
async fn test_models_slash_error_sanitizes_provider_body() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(503).set_body_string(
            "catalog unavailable \u{1b}]8;;https://evil.invalid\u{7}click\u{1b}]8;;\u{7}",
        ))
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
chat_path = "{CHAT_PATH}"
models_path = "/v1/models"
model = "example-model"
context_window = 8192
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let script = home.path().join("models-error.txt");
    fs::write(&script, "/models\n").unwrap();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.args(["chat", "--script", &script.to_string_lossy()]);
        cmd.assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains("catalog unavailable"))
            .stderr(predicate::str::contains("\u{1b}").not())
            .stderr(predicate::str::contains("https://evil.invalid").not());
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jsonl_script_models_sigint_closes_command_frame() {
    use std::process::Stdio;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_json(serde_json::json!({"data": []})),
        )
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
chat_path = "{CHAT_PATH}"
models_path = "/v1/models"
model = "example-model"
context_window = 8192
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let script = home.path().join("models-cancel.txt");
    fs::write(&script, "/models\n").unwrap();
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("kitsu"));
    cmd.env("HOME", home.path())
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .args(["--jsonl", "chat", "--script", &script.to_string_lossy()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().unwrap();
    let pid = child.id();
    for _ in 0..100 {
        if !server.received_requests().await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!server.received_requests().await.unwrap().is_empty());
    // SAFETY: `pid` is the live child launched immediately above.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) }, 0);
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(130));
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds: Vec<_> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "run_started",
            "command_started",
            "error",
            "command_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[3]["status"], "cancelled");
    assert_eq!(events[4]["status"], "cancelled");
}

/// `/context` prints the model, resolved window, and a per-category breakdown —
/// no gateway call is needed for a context-only script.
#[tokio::test]
#[serial]
async fn test_context_slash_command_shows_utilization() {
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
model = "test-model"
context_window = 8000
auth = { method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN" }
"#,
    )
    .unwrap();

    let script_path = home.path().join("scn.txt");
    fs::write(&script_path, "/context\n").unwrap();
    let script_str = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["--gateway", "example", "chat", "--script", &script_str]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("window: 8000"))
            .stderr(predicate::str::contains("effective"))
            // provenance table: source/policy/why columns + the base prompt row.
            .stderr(predicate::str::contains("base prompt"))
            .stderr(predicate::str::contains("policy"))
            .stderr(predicate::str::contains("why"));
    })
    .await
    .expect("join");
}

/// `/status` and `/permissions` expose the live session without making a model
/// request, including the selected gateway (not an ambiguous "default" label).
#[tokio::test]
#[serial]
async fn test_status_and_permissions_slash_commands() {
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
model = "test-model"
context_window = 8000
auth = { method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN" }
"#,
    )
    .unwrap();

    let script_path = home.path().join("scn.txt");
    fs::write(&script_path, "/status\n/permissions\n/quit\n").unwrap();
    let script_str = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["--gateway", "example", "chat", "--script", &script_str]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("session:"))
            .stderr(predicate::str::contains("gateway      example"))
            .stderr(predicate::str::contains("model        test-model"))
            .stderr(predicate::str::contains("context"))
            .stderr(predicate::str::contains("permissions:"))
            .stderr(predicate::str::contains("mode         deny"))
            .stderr(predicate::str::contains("read roots   none"))
            .stderr(predicate::str::contains("write roots  none"));
    })
    .await
    .expect("join");
}

/// `/skills` discovers and lists a LOCAL project skill (index end-to-end). No
/// gateway call needed for a skills-only script.
#[tokio::test]
#[serial]
async fn test_skills_slash_lists_local_skills() {
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
model = "test-model"
context_window = 8000
auth = { method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN" }
"#,
    )
    .unwrap();

    // A project dir (with .git so discovery stops there) holding a skill.
    let work = TempDir::new().expect("work dir");
    fs::create_dir_all(work.path().join(".git")).unwrap();
    let sk = work.path().join(".kitsu/skills/greet");
    fs::create_dir_all(&sk).unwrap();
    fs::write(
        sk.join("SKILL.md"),
        "---\nname: greet\ndescription: say hello nicely\n---\nSay hello.",
    )
    .unwrap();

    let script = work.path().join("s.txt");
    fs::write(&script, "/skills\n").unwrap();
    let script_str = script.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.env_remove("KITSU_NO_PROJECT_CONTEXT"); // allow skills to load
        cmd.current_dir(&work_path);
        cmd.args(["--gateway", "example", "chat", "--script", &script_str]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("greet"))
            .stderr(predicate::str::contains("say hello nicely"))
            .stderr(predicate::str::contains("skill(s) available"));
    })
    .await
    .expect("join");
}

/// `KITSU_NO_PROJECT_CONTEXT=1` (set by `base_cmd`) disables skill discovery.
#[tokio::test]
#[serial]
async fn test_skills_disabled_by_env() {
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
model = "test-model"
context_window = 8000
auth = { method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN" }
"#,
    )
    .unwrap();

    let work = TempDir::new().expect("work dir");
    fs::create_dir_all(work.path().join(".git")).unwrap();
    let sk = work.path().join(".kitsu/skills/greet");
    fs::create_dir_all(&sk).unwrap();
    fs::write(
        sk.join("SKILL.md"),
        "---\nname: greet\ndescription: hi\n---\nHi.",
    )
    .unwrap();

    let script = work.path().join("s.txt");
    fs::write(&script, "/skills\n").unwrap();
    let script_str = script.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path); // sets KITSU_NO_PROJECT_CONTEXT=1
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.current_dir(&work_path);
        cmd.args(["--gateway", "example", "chat", "--script", &script_str]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("no skills found"));
    })
    .await
    .expect("join");
}

/// `ask --skill <name>` preloads the skill body into the request even on the
/// tool-less `ask` path — and the body actually reaches the model.
#[tokio::test]
#[serial]
async fn test_ask_skill_preloaded_reaches_request() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());

    // Local project skill with a distinctive body marker.
    let work = TempDir::new().expect("work dir");
    fs::create_dir_all(work.path().join(".git")).unwrap();
    let sk = work.path().join(".kitsu/skills/greet");
    fs::create_dir_all(&sk).unwrap();
    fs::write(
        sk.join("SKILL.md"),
        "---\nname: greet\ndescription: hi\n---\nSKILL_BODY_MARKER_greet",
    )
    .unwrap();

    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.env_remove("KITSU_NO_PROJECT_CONTEXT");
        cmd.current_dir(&work_path);
        cmd.args(["--gateway", "example", "--skill", "greet", "ask", "hello"]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("preloaded 1 skill(s)"));
    })
    .await
    .expect("join");

    // The skill body must have been sent in the request.
    let reqs = server.received_requests().await.expect("recorded requests");
    let body = String::from_utf8_lossy(&reqs[0].body);
    assert!(
        body.contains("SKILL_BODY_MARKER_greet"),
        "the preloaded skill body must reach the model"
    );
}

/// Regression: `/model <name>` must re-resolve the context window for the NEW
/// model instead of keeping the OLD model's (session-start-discovered) window.
/// A stale, larger window would under-trim relative to the new model's real
/// limit and let the gateway reject an oversized request instead of kitsu
/// proactively trimming to fit.
#[tokio::test]
#[serial]
async fn test_model_switch_reresolves_context_window() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // Both models are served by the SAME /v1/models catalog (typical of one
    // gateway), with distinct context lengths — switching models must pick up
    // the new entry, not keep reporting the old one.
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"object":"list","data":[{"id":"alpha-model","context_length":4096},{"id":"beta-model","context_length":32768}]}"#,
        ))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{base_url}"
chat_path = "{CHAT_PATH}"
models_path = "/v1/models"
model = "alpha-model"
auth = {{ method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN" }}
"#
        ),
    )
    .unwrap();

    let script = "/model beta-model\n/context\n";
    let script_path = home.path().join("scn.txt");
    fs::write(&script_path, script).unwrap();
    let script_str = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["--gateway", "example", "chat", "--script", &script_str]);
        cmd.assert()
            .success()
            // The switch itself reports the freshly-discovered window...
            .stderr(predicate::str::contains("switched to beta-model"))
            .stderr(predicate::str::contains("context window: 32768"))
            // ...and /context reflects it too, not the stale alpha-model window.
            .stderr(predicate::str::contains("window: 32768"))
            .stderr(predicate::str::contains("window: 4096").not());
    })
    .await
    .expect("join");
}

/// Regression: an explicit `context_window` (equivalent to `--context-window`)
/// is a user/config declaration that overrides discovery entirely — `/model`
/// must NOT silently replace it with a freshly `/v1/models`-discovered value.
#[tokio::test]
#[serial]
async fn test_model_switch_keeps_pinned_context_window() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // If the pin were ignored, /model would discover THIS (different) value.
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"object":"list","data":[{"id":"alpha-model","context_length":4096},{"id":"beta-model","context_length":9999}]}"#,
        ))
        .mount(&server)
        .await;

    let base_url = server.uri();
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{base_url}"
chat_path = "{CHAT_PATH}"
models_path = "/v1/models"
model = "alpha-model"
context_window = 1234
auth = {{ method = "bearer-env", env = "ANTHROPIC_AUTH_TOKEN" }}
"#
        ),
    )
    .unwrap();

    let script = "/model beta-model\n/context\n";
    let script_path = home.path().join("scn.txt");
    fs::write(&script_path, script).unwrap();
    let script_str = script_path.to_string_lossy().to_string();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["--gateway", "example", "chat", "--script", &script_str]);
        cmd.assert()
            .success()
            .stderr(predicate::str::contains("switched to beta-model"))
            .stderr(predicate::str::contains("pinned"))
            // /context must still show the pinned window, never the discovered one.
            .stderr(predicate::str::contains("window: 1234"))
            .stderr(predicate::str::contains("window: 9999").not());
    })
    .await
    .expect("join");
}
