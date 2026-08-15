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

#[test]
fn test_empty_durable_script_does_not_create_a_session_snapshot() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("empty-durable.script");
    fs::write(&script, "\n# no turns\n").unwrap();

    let mut chat = base_cmd(home.path());
    chat.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["chat", "--session", "--script", &script.to_string_lossy()])
        .assert()
        .success();

    let mut sessions = base_cmd(home.path());
    let output = sessions
        .args(["sessions", "--json"])
        .output()
        .expect("list sessions after empty durable script");
    assert!(output.status.success(), "{:?}", output);
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed, serde_json::json!([]));
}

#[test]
fn test_query_only_durable_script_does_not_create_a_session_snapshot() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("query-only-durable.script");
    fs::write(&script, "/status\n/sessions\n/quit\n").unwrap();

    let mut chat = base_cmd(home.path());
    chat.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["chat", "--session", "--script", &script.to_string_lossy()])
        .assert()
        .success();

    let mut sessions = base_cmd(home.path());
    let output = sessions
        .args(["sessions", "--json"])
        .output()
        .expect("list sessions after query-only durable script");
    assert!(output.status.success(), "{output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed, serde_json::json!([]));
}

#[cfg(unix)]
#[test]
#[serial]
fn test_query_only_interactive_session_does_not_create_a_snapshot() {
    use std::io::Write as _;
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");

    let mut master = -1;
    let mut slave = -1;
    // SAFETY: both output pointers are valid for this call; null termios and
    // winsize pointers request the platform defaults.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0,
        "allocate a test PTY"
    );
    // SAFETY: `slave` is the live descriptor returned by openpty. Each dup is
    // independently owned by the matching Stdio below.
    let child_stdin = unsafe { libc::dup(slave) };
    // SAFETY: see above.
    let child_stdout = unsafe { libc::dup(slave) };
    assert!(child_stdin >= 0 && child_stdout >= 0);

    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
    cmd.env("HOME", home.path())
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        // SAFETY: the duplicated descriptors are valid and transferred to
        // their Stdio owners exactly once.
        .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
        // SAFETY: see above.
        .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
        // SAFETY: `slave` remains valid and ownership transfers here.
        .stderr(unsafe { Stdio::from_raw_fd(slave) })
        .arg("chat");
    let mut child = cmd.spawn().expect("spawn interactive chat on PTY");
    // SAFETY: `master` is the remaining live descriptor returned by openpty
    // and is transferred to this File exactly once.
    let mut terminal = unsafe { fs::File::from_raw_fd(master) };
    terminal
        .write_all(b"/status\n/quit\n")
        .expect("submit query-only interactive commands");
    terminal.flush().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll interactive chat") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("query-only interactive chat did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        status.success(),
        "interactive query-only chat failed: {status}"
    );

    let mut sessions = base_cmd(home.path());
    let output = sessions
        .args(["sessions", "--json"])
        .output()
        .expect("list sessions after query-only interactive chat");
    assert!(output.status.success(), "{output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed, serde_json::json!([]));
}

#[test]
fn test_init_is_no_clobber_and_doctor_is_read_only_json() {
    let home = TempDir::new().expect("temp home");
    let config = home.path().join("project").join("kitsu.toml");

    let mut init = base_cmd(home.path());
    let output = init
        .args(["init", "--path", &config.to_string_lossy(), "--json"])
        .output()
        .expect("run init");
    assert!(output.status.success(), "{:?}", output);
    let initialized: serde_json::Value = serde_json::from_slice(&output.stdout).expect("init JSON");
    assert_eq!(initialized["created"], true);
    assert_eq!(initialized["contains_credentials"], false);
    assert_eq!(initialized["next"][2], config.to_string_lossy().as_ref());
    let contents = fs::read_to_string(&config).expect("starter config");
    assert!(contents.contains("auth = { method = \"none\" }"));
    assert!(!contents.to_lowercase().contains("token ="));
    assert!(contents.contains(&format!(
        "# Next: kitsu --config {} doctor",
        config.display()
    )));
    assert!(!contents.contains("--config ./kitsu.toml"));

    let mut second = base_cmd(home.path());
    second
        .args(["init", "--path", &config.to_string_lossy()])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("refusing to overwrite"));
    assert_eq!(
        fs::read_to_string(&config).expect("unchanged config"),
        contents
    );

    // The config is deliberately outside ~/.kitsu. Offline doctor must inspect
    // the prospective isolated state namespace without creating ~/.kitsu.
    assert!(!home.path().join(".kitsu").exists());
    let mut doctor = base_cmd(home.path());
    let output = doctor
        .args(["--config", &config.to_string_lossy(), "doctor", "--json"])
        .output()
        .expect("run doctor");
    assert!(output.status.success(), "{:?}", output);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["read_only"], true);
    assert_eq!(report["config"]["parse_ok"], true);
    assert_eq!(report["gateway"]["name"], "ollama");
    assert_eq!(report["auth"]["credential_required"], false);
    assert_eq!(
        report["auth"]["credential_present"],
        serde_json::Value::Null
    );
    assert_eq!(report["online"]["requested"], false);
    assert!(!home.path().join(".kitsu").exists());
}

/// Cycle-8 docs-drift finding: `kitsu init` used to emit `flavor = "ollama"`
/// and `127.0.0.1`, disagreeing with every README provider recipe (which all
/// use `flavor = "openai"`), and defaulted to a model tag
/// (`qwen2.5-coder:7b`) that 404s against what `ollama pull qwen2.5-coder`
/// actually produces. The starter config must now match README's "Ollama
/// (local)" recipe exactly on the fields that matter (flavor, base_url,
/// chat_path, models_path, model, auth).
#[test]
fn test_init_generated_config_matches_the_readme_ollama_recipe() {
    let home = TempDir::new().expect("temp home");
    let config = home.path().join("kitsu.toml");
    let mut init = base_cmd(home.path());
    init.args(["init", "--path", &config.to_string_lossy()])
        .assert()
        .success();
    let contents = fs::read_to_string(&config).expect("starter config");
    let parsed: toml::Value = contents.parse().expect("starter config is valid TOML");
    let gateway = &parsed["gateways"]["ollama"];
    assert_eq!(gateway["flavor"].as_str(), Some("openai"));
    assert_eq!(gateway["base_url"].as_str(), Some("http://localhost:11434"));
    assert_eq!(gateway["chat_path"].as_str(), Some("/v1/chat/completions"));
    assert_eq!(gateway["models_path"].as_str(), Some("/v1/models"));
    assert_eq!(gateway["model"].as_str(), Some("llama3.2"));
    assert_eq!(gateway["auth"]["method"].as_str(), Some("none"));
}

/// The generated config's non-`base_url` fields (flavor, paths, auth) must
/// actually produce a working request against an OpenAI-compatible endpoint
/// — the shape Ollama serves. This swaps only `base_url` to a local mock
/// (standing in for a real `ollama serve`) and otherwise runs the file
/// `kitsu init` wrote, byte-for-byte, through `kitsu ask`.
#[tokio::test]
#[serial]
async fn test_init_generated_config_works_end_to_end_against_an_ollama_shaped_endpoint() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "hi from ollama-shaped mock"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    let config = home.path().join("kitsu.toml");
    let mut init = base_cmd(home.path());
    init.args(["init", "--path", &config.to_string_lossy()])
        .assert()
        .success();
    let generated = fs::read_to_string(&config).expect("starter config");
    let live = generated.replace("http://localhost:11434", &server.uri());
    assert_ne!(live, generated, "base_url substitution must actually apply");
    fs::write(&config, live).unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.args([
        "--config",
        &config.to_string_lossy(),
        "--no-stream",
        "ask",
        "hi",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("hi from ollama-shaped mock"));
    server.verify().await;
}

#[test]
fn test_doctor_never_echoes_static_or_malformed_config_secrets() {
    let home = TempDir::new().expect("temp home");
    let config = home.path().join("doctor.toml");
    const SECRET: &str = "DOCTOR_SECRET_MUST_NOT_LEAK";
    fs::write(
        &config,
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = ""
model = "test-model"
context_window = 8192
auth = {{ method = "static", token = "{SECRET}" }}
"#
        ),
    )
    .unwrap();

    let mut valid = base_cmd(home.path());
    let output = valid
        .args(["--config", &config.to_string_lossy(), "doctor", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(SECRET));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["auth"]["credential_present"], true);
    assert!(report["auth"]["status"]
        .as_str()
        .unwrap()
        .contains("value hidden"));

    fs::write(
        &config,
        format!("default_gateway = \"example\" trailing {SECRET}\n"),
    )
    .unwrap();
    let mut malformed = base_cmd(home.path());
    let output = malformed
        .args(["--config", &config.to_string_lossy(), "doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(SECRET));
    assert!(combined.contains("source excerpt withheld"));
}

#[test]
fn test_malformed_config_never_echoes_credentials_in_any_chat_surface() {
    let home = TempDir::new().expect("temp home");
    let config = home.path().join("malformed-credentials.toml");
    const SECRETS: [&str; 4] = [
        "hf_KitsuMalformedCredential123",
        "sk-KitsuMalformedCredential123",
        "api-key=KitsuMalformedCredential123",
        "token=KitsuMalformedCredential123",
    ];
    // toml's native diagnostic includes this entire source line. Each command
    // must instead render only location metadata and the withheld-source note.
    fs::write(
        &config,
        format!(
            "default_gateway = \"example\"\ncredential = \"{}\" \"{}\" \"{}\" \"{}\"\n",
            SECRETS[0], SECRETS[1], SECRETS[2], SECRETS[3]
        ),
    )
    .expect("write malformed config");

    let invocations: [(&str, &[&str]); 4] = [
        ("ask", &["ask", "hello"]),
        ("chat", &["chat"]),
        ("review", &["review"]),
        ("models", &["models"]),
    ];
    for jsonl in [false, true] {
        for (name, args) in invocations {
            let mut cmd = base_cmd(home.path());
            cmd.arg("--config").arg(&config);
            if jsonl {
                cmd.arg("--jsonl");
            }
            let output = cmd.args(args).output().expect("run kitsu");
            assert_eq!(
                output.status.code(),
                Some(1),
                "{name} (jsonl={jsonl}) unexpectedly succeeded: {output:?}"
            );
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            for secret in SECRETS {
                assert!(
                    !combined.contains(secret),
                    "{name} (jsonl={jsonl}) leaked {secret}: {combined}"
                );
            }
            assert!(
                combined.contains("TOML parse failed at line 2"),
                "{name} (jsonl={jsonl}) lost safe location context: {combined}"
            );
            assert!(
                combined.contains("source excerpt withheld"),
                "{name} (jsonl={jsonl}) omitted redaction notice: {combined}"
            );
            if jsonl {
                assert!(
                    output.stderr.is_empty(),
                    "JSONL diagnostic escaped to stderr: {combined}"
                );
            } else {
                assert!(
                    output.stdout.is_empty(),
                    "human diagnostic escaped to stdout: {combined}"
                );
            }
        }
    }
}

#[test]
fn test_sessions_missing_namespace_is_provider_free_and_friendly() {
    let home = TempDir::new().expect("temp home");
    let missing_config = home.path().join("does-not-exist.toml");

    let mut human = base_cmd(home.path());
    human
        .args(["--config", &missing_config.to_string_lossy(), "sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no saved sessions for config"));

    let mut json = base_cmd(home.path());
    let output = json
        .args([
            "--config",
            &missing_config.to_string_lossy(),
            "sessions",
            "--json",
        ])
        .output()
        .expect("list sessions");
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([])
    );
    assert!(!home.path().join(".kitsu").exists());

    let mut show = base_cmd(home.path());
    show.args([
        "--config",
        &missing_config.to_string_lossy(),
        "sessions",
        "show",
        "last",
    ])
    .assert()
    .failure()
    .code(1)
    .stderr(predicate::str::contains(
        "no saved sessions in this config directory",
    ));
}

#[test]
fn test_script_session_flags_reject_ambiguous_no_session() {
    let home = TempDir::new().expect("temp home");
    let script = home.path().join("empty.txt");
    fs::write(&script, "").unwrap();

    let mut new_session = base_cmd(home.path());
    new_session
        .args([
            "chat",
            "--session",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("cannot be used with"));

    let mut resume = base_cmd(home.path());
    resume
        .args([
            "chat",
            "--resume",
            "last",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("cannot be used with"));
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
async fn test_scripted_session_continuity_and_provider_free_lifecycle() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"saved answer"},"finish_reason":null}]}"#,
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
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let first_script = home.path().join("first.txt");
    let second_script = home.path().join("second.txt");
    let export = home.path().join("fork.json");
    fs::write(&first_script, "first saved turn\n").unwrap();
    fs::write(&second_script, "second saved turn\n").unwrap();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut first = base_cmd(&home_path);
        let output = first
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "chat",
                "--session",
                "--allow-path",
                &home_path.to_string_lossy(),
                "--script",
                &first_script.to_string_lossy(),
            ])
            .output()
            .expect("first scripted session");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("autosave planned"));

        let mut list = base_cmd(&home_path);
        let output = list
            .args(["sessions", "--json"])
            .output()
            .expect("list sessions");
        assert!(output.status.success(), "{:?}", output);
        let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let id = listed[0]["id"].as_str().expect("session id").to_string();
        assert_eq!(listed[0]["turns"], 1);

        let mut second = base_cmd(&home_path);
        let output = second
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "chat",
                "--resume",
                &id,
                "--script",
                &second_script.to_string_lossy(),
            ])
            .output()
            .expect("resume scripted session");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("saved grants were not restored"));
        assert!(stderr.contains("invocation grants retained: 0 read, 0 write"));

        let mut show = base_cmd(&home_path);
        let output = show
            .args(["sessions", "--json", "show", &id])
            .output()
            .expect("show session");
        assert!(output.status.success(), "{:?}", output);
        let saved: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(saved["id"], id);
        assert!(saved.get("permissions").is_none());
        let saved_cwd = saved["directory"].as_str().unwrap().to_string();
        let transcript = saved["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(transcript.contains("first saved turn"));
        assert!(transcript.contains("second saved turn"));

        // In a non-TTY, resume is a provider-free command resolver rather than
        // an accidental chat launch. Resolve it from outside the saved cwd so
        // the returned launch contract must carry that cwd explicitly.
        let mut resume = base_cmd(&home_path);
        let output = resume
            .current_dir(&home_path)
            .args(["sessions", "--json", "resume", "last"])
            .output()
            .expect("resolve resume command");
        assert!(output.status.success(), "{:?}", output);
        let resume: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(resume["launched"], false);
        assert_eq!(resume["session_id"], id);
        assert_eq!(resume["cwd"], saved_cwd);
        assert_eq!(resume["argv"][3], "chat");

        #[cfg(unix)]
        {
            let mut human_resume = base_cmd(&home_path);
            let output = human_resume
                .current_dir(&home_path)
                .args(["sessions", "resume", "last"])
                .output()
                .expect("render runnable resume command");
            assert!(output.status.success(), "{:?}", output);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("resume with: cd "), "{stdout}");
            assert!(stdout.contains(" && kitsu --config "), "{stdout}");
            assert!(stdout.contains("working directory: "), "{stdout}");
            assert!(stdout.contains(&saved_cwd), "{stdout}");
        }

        let mut fork = base_cmd(&home_path);
        let output = fork
            .args(["sessions", "--json", "fork", &id])
            .output()
            .expect("fork session");
        assert!(output.status.success(), "{:?}", output);
        let forked: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let forked_id = forked["session"]["id"].as_str().unwrap().to_string();
        assert_ne!(forked_id, id);
        assert_eq!(forked["permissions_restored"], false);

        let mut export_cmd = base_cmd(&home_path);
        export_cmd
            .args([
                "sessions",
                "--json",
                "export",
                &forked_id,
                &export.to_string_lossy(),
                "--format",
                "json",
            ])
            .assert()
            .success();
        assert!(export.is_file());

        // No --format: the destination extension decides, and says so.
        let inferred = home_path.join("inferred-export.json");
        let mut inferred_cmd = base_cmd(&home_path);
        inferred_cmd
            .args([
                "sessions",
                "export",
                &forked_id,
                &inferred.to_string_lossy(),
            ])
            .assert()
            .success()
            .stderr(predicate::str::contains(
                "inferred --format json from the .json destination",
            ));
        serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&inferred).unwrap())
            .expect("inferred export is a JSON snapshot");

        // Anything else keeps the Markdown default, silently.
        let markdown = home_path.join("inferred-export.md");
        let mut markdown_cmd = base_cmd(&home_path);
        markdown_cmd
            .args([
                "sessions",
                "export",
                &forked_id,
                &markdown.to_string_lossy(),
            ])
            .assert()
            .success()
            .stderr(predicate::str::contains("inferred --format").not());
        assert!(
            serde_json::from_str::<serde_json::Value>(&fs::read_to_string(&markdown).unwrap())
                .is_err()
        );

        let mut archive = base_cmd(&home_path);
        archive
            .args(["sessions", "archive", &forked_id])
            .assert()
            .success();
        let mut restore = base_cmd(&home_path);
        restore
            .args(["sessions", "restore", &forked_id])
            .assert()
            .success();
        let mut delete = base_cmd(&home_path);
        delete
            .args(["sessions", "delete", &forked_id])
            .assert()
            .success();

        let mut final_list = base_cmd(&home_path);
        let output = final_list
            .args(["sessions", "--json"])
            .output()
            .expect("final list");
        let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["id"], id);
    })
    .await
    .expect("spawned session lifecycle should join");

    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_doctor_online_reachability_is_explicit_and_machine_readable() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"id": "doctor-model"},
                {"id": " doctor-model "},
                {"id": ""},
                {"id": "   "}
            ]
        })))
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    let config = home.path().join("doctor-online.toml");
    fs::write(
        &config,
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
model = "doctor-model"
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut doctor = base_cmd(&home_path);
        let output = doctor
            .args([
                "--config",
                &config.to_string_lossy(),
                "doctor",
                "--online",
                "--json",
            ])
            .output()
            .expect("run online doctor");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["online"]["requested"], true);
        assert_eq!(report["online"]["status"], "reachable");
        assert_eq!(report["online"]["models"], 1);
        assert_eq!(report["online"]["effective_model"], "doctor-model");
        assert_eq!(report["online"]["effective_model_available"], true);

        let mut missing = base_cmd(&home_path);
        let output = missing
            .args([
                "--config",
                &config.to_string_lossy(),
                "--model",
                "Doctor-Model",
                "doctor",
                "--online",
                "--json",
            ])
            .output()
            .expect("run online doctor with absent exact model");
        assert_eq!(output.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["online"]["status"], "reachable");
        assert_eq!(report["online"]["models"], 1);
        assert_eq!(report["online"]["effective_model"], "Doctor-Model");
        assert_eq!(report["online"]["effective_model_available"], false);
        assert!(report["blocking_issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue.as_str().is_some_and(|issue| {
                issue.contains("not advertised by the normalized gateway catalog")
            })));
    })
    .await
    .expect("online doctor process should join");

    server.verify().await;
}

#[test]
fn test_doctor_online_requested_but_unavailable_is_not_success() {
    let home = TempDir::new().expect("temp home");
    let config = home.path().join("doctor-no-catalog.toml");
    fs::write(
        &config,
        r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = ""
model = "doctor-model"
auth = { method = "none" }
"#,
    )
    .unwrap();

    let mut doctor = base_cmd(home.path());
    let output = doctor
        .args([
            "--config",
            &config.to_string_lossy(),
            "doctor",
            "--online",
            "--json",
        ])
        .output()
        .expect("run skipped online doctor");
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["online"]["requested"], true);
    assert!(report["online"]["status"]
        .as_str()
        .unwrap()
        .contains("requested but skipped"));
    assert!(report["blocking_issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue
            .as_str()
            .is_some_and(|issue| issue.contains("requested but skipped"))));
}

#[tokio::test]
#[serial]
async fn test_jsonl_ask_is_versioned_framed_and_parseable() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let completion = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"json "},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(completion),
        )
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args(["--jsonl", "--reasoning-effort", "high", "ask", "hello"]);
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
        let assistant_index = events
            .iter()
            .position(|event| event["type"] == "assistant")
            .expect("final assistant event");
        assert_eq!(events[assistant_index]["text"], "json answer");
        let deltas = events
            .iter()
            .enumerate()
            .filter(|(_, event)| event["type"] == "assistant_delta")
            .map(|(index, event)| {
                assert!(
                    index < assistant_index,
                    "delta must precede final assistant"
                );
                event["text"].as_str().unwrap()
            })
            .collect::<String>();
        assert_eq!(deltas, events[assistant_index]["text"]);
        let disposition_index = events
            .iter()
            .position(|event| event["type"] == "assistant_disposition")
            .expect("terminal assistant disposition");
        assert!(assistant_index < disposition_index);
        assert_eq!(events[disposition_index]["status"], "committed");
        assert_eq!(events.last().unwrap()["type"], "final_summary");
        assert_eq!(events.last().unwrap()["status"], "ok");
        assert!(!stdout.lines().any(|line| !line.starts_with('{')));
        assert!(
            output.stderr.is_empty(),
            "routine diagnostics leaked to JSONL stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    })
    .await
    .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        request["stream"], true,
        "JSONL must preserve provider streaming"
    );
    assert_eq!(request["reasoning_effort"], "high");
}

#[tokio::test]
#[serial]
async fn test_jsonl_partial_stream_failure_discards_provisional_assistant_output() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Write two HTTP chunks with a gap so the first assistant fragment reaches
    // the runtime observer before malformed provider data fails the stream.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0, "client closed before sending request headers");
            request.extend_from_slice(&buffer[..read]);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let first = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"provisional\"},\"finish_reason\":null}]}\n\n";
        socket
            .write_all(format!("{:x}\r\n", first.len()).as_bytes())
            .await
            .unwrap();
        socket.write_all(first).await.unwrap();
        socket.write_all(b"\r\n").await.unwrap();
        socket.flush().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let malformed = b"data: {not-json}\n\n";
        socket
            .write_all(format!("{:x}\r\n", malformed.len()).as_bytes())
            .await
            .unwrap();
        socket.write_all(malformed).await.unwrap();
        socket.write_all(b"\r\n0\r\n\r\n").await.unwrap();
    });

    let home = TempDir::new().unwrap();
    write_config(home.path(), &format!("http://{address}"));
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--jsonl", "ask", "fail after a fragment"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let delta = events
        .iter()
        .position(|event| event["type"] == "assistant_delta" && event["text"] == "provisional")
        .unwrap_or_else(|| panic!("provisional fragment missing: {events:#?}"));
    let disposition = events
        .iter()
        .position(|event| event["type"] == "assistant_disposition")
        .expect("discard disposition");
    let error = events
        .iter()
        .position(|event| event["type"] == "error")
        .expect("provider error");
    assert!(delta < disposition && disposition < error);
    assert_eq!(events[disposition]["status"], "discarded");
    assert!(!events.iter().any(|event| event["type"] == "assistant"));
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert_eq!(events.last().unwrap()["status"], "error");
    server.await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_jsonl_script_happy_path_has_no_transcript_or_routine_stderr() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "quiet answer"},
                "finish_reason": "stop"
            }]
        })))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("quiet-jsonl.txt");
    fs::write(&script, "/context\nhello from script\n").unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().into_owned();

    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--script",
            &script_path,
            "--no-session",
        ]);
        cmd.output().unwrap()
    })
    .await
    .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "JSONL script leaked human stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    assert!(events
        .iter()
        .any(|event| { event["type"] == "assistant" && event["text"] == "quiet answer" }));
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert_eq!(events.last().unwrap()["status"], "ok");
}

#[cfg(target_os = "linux")]
#[test]
#[serial]
fn test_jsonl_output_failure_on_dev_full_is_nonzero_and_actionable() {
    use std::process::Stdio;

    let home = TempDir::new().unwrap();
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("Linux /dev/full");
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
    cmd.env("HOME", home.path())
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .args(["--jsonl", "--max-rounds", "not-a-number", "ask", "hello"])
        .stdout(Stdio::from(full));

    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSONL output failed") && stderr.contains("write failed"),
        "unexpected stderr: {stderr}"
    );
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
            "assistant_disposition",
            "error",
            "turn_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[3]["status"], "discarded");
    assert_eq!(events[4]["exit_code"], 1);
    assert_eq!(events[6]["user_failures"], 1);
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
            "provider_attempt",
            "assistant_disposition",
            "error",
            "turn_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[3]["category"], "cancelled");
    assert_eq!(events[3]["outcome"], "cancelled");
    assert_eq!(events[3]["terminal"], true);
    assert_eq!(events[4]["status"], "discarded");
    assert_eq!(events[5]["kind"], "cancelled");
    assert_eq!(events[5]["exit_code"], 130);
    assert_eq!(events[6]["status"], "cancelled");
    assert_eq!(events[7]["status"], "cancelled");
    assert_eq!(events[7]["cancelled_failures"], 1);
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
            // The one-shot evidence disclosure is framed inside the command that
            // caused it, before the prompt that carries the bytes.
            "command_result",
            "turn_started",
            "provider_request",
            "provider_attempt",
            "assistant_disposition",
            "error",
            "turn_finished",
            "command_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[2]["result"]["kind"], "review_disclosure");
    assert_eq!(events[2]["result"]["model_disclosed"], true);
    assert_eq!(events[2]["result"]["model_read_root_granted"], false);
    assert!(events[2]["result"]["evidence_bytes"].as_u64().unwrap() > 0);
    assert_eq!(events[5]["category"], "cancelled");
    assert_eq!(events[5]["outcome"], "cancelled");
    assert_eq!(events[6]["status"], "discarded");
    assert_eq!(events[7]["kind"], "cancelled");
    assert_eq!(events[8]["status"], "cancelled");
    assert_eq!(events[9]["status"], "cancelled");
    assert_eq!(events[10]["status"], "cancelled");
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

/// A positional chat prompt is authoritative input and must not wait for an
/// unrelated piped stdin handle to reach EOF.
#[tokio::test]
#[serial]
async fn test_chat_positional_prompt_ignores_held_open_stdin() {
    use std::process::Stdio;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("positional prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "positional complete"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("kitsu"));
    cmd.env("HOME", home.path())
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--no-stream",
            "--timeout-secs",
            "2",
            "chat",
            "positional prompt",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let held_stdin = child.stdin.take().expect("keep stdin open");
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::task::spawn_blocking(move || child.wait_with_output().unwrap()),
    )
    .await
    .expect("positional prompt must not wait for stdin EOF")
    .unwrap();
    drop(held_stdin);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("positional complete"));
    server.verify().await;
}

/// Piped prompt acquisition consumes the same bounded one-shot budget as the
/// later provider turn rather than waiting forever or starting a fresh timer.
#[tokio::test]
#[serial]
async fn test_chat_piped_input_obeys_end_to_end_deadline() {
    use std::process::Stdio;

    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("kitsu"));
    cmd.env("HOME", home.path())
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["--jsonl", "--timeout-secs", "1", "chat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let held_stdin = child.stdin.take().expect("keep stdin open");
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::task::spawn_blocking(move || child.wait_with_output().unwrap()),
    )
    .await
    .expect("piped input must be bounded by --timeout-secs")
    .unwrap();
    drop(held_stdin);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.first().unwrap()["type"], "run_started");
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .is_some_and(|message| message.contains("end-to-end deadline"))
    }));
    assert_eq!(events.last().unwrap()["type"], "final_summary");
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

/// The explicit combined capability supports read-modify-write workflows
/// without making the plainly named `--allow-write` flag disclose old content.
#[tokio::test]
#[serial]
async fn test_allow_read_write_supports_readback() {
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
        cmd.args(["chat", "--gateway", "example", "--allow-read-write", &allow]);
        cmd.write_stdin("read the note\n");
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("SECRET_MARKER_43"));
    })
    .await
    .expect("spawned kitsu chat tool round should join");
    drop(work);
}

#[test]
fn test_allow_write_registers_only_write_capability() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let work = TempDir::new().expect("write root");
    let script = home.path().join("permissions.txt");
    fs::write(&script, "/tools\n/permissions\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "chat",
        "--allow-write",
        &work.path().to_string_lossy(),
        "--script",
        &script.to_string_lossy(),
    ]);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains("fs_write_file"))
        .stderr(predicate::str::contains("fs_read_file").not())
        .stderr(predicate::str::contains("fs_edit_file").not())
        .stderr(predicate::str::contains("read roots   none"))
        .stderr(predicate::str::contains("(invocation/base)"));
}

#[tokio::test]
#[serial]
async fn test_successful_undo_is_a_trusted_event_in_the_next_provider_request() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().expect("work dir");
    let target = work.path().join("undo-target.txt");
    let tool_args = serde_json::json!({
        "path": target.to_string_lossy(),
        "content": "created before undo"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "undo_write_1",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": tool_args}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "continued"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("undo.script");
    fs::write(
        &script,
        "create the requested file\n/undo\ncontinue after restoring it\n",
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();
    let script_path = script.clone();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        let output = cmd
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--gateway",
                "example",
                "--no-stream",
                "chat",
                "--allow-write",
                &work_path.to_string_lossy(),
                "--script",
                &script_path.to_string_lossy(),
            ])
            .output()
            .expect("run scripted undo");
        assert!(
            output.status.success(),
            "stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    })
    .await
    .expect("scripted undo process should join");

    assert!(
        !target.exists(),
        "undo should remove the newly created file"
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let body: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    let undo_event = messages
        .iter()
        .find(|message| {
            message["role"] == "system"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("trusted filesystem event: undo"))
        })
        .expect("next request should contain the trusted undo event");
    let content = undo_event["content"].as_str().unwrap();
    assert!(content.contains("Checkpoint #"));
    assert!(content.contains(target.to_string_lossy().as_ref()));
    assert!(content.contains("Re-read every affected path"));
    assert!(messages.iter().any(|message| {
        message["role"] == "user" && message["content"] == "continue after restoring it"
    }));
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_yes_cannot_write_outside_explicit_root() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let allowed = TempDir::new().expect("allowed root");
    let outside = TempDir::new().expect("outside root");
    let escaped = outside.path().join("must-not-exist.txt");
    let tool_args = format!(
        r#"{{\"path\":\"{}\",\"content\":\"escaped\"}}"#,
        escaped.to_string_lossy().replace('\\', "\\\\")
    );
    let request_write = [
        format!(
            r#"data: {{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":"call_write","type":"function","function":{{"name":"fs_write_file","arguments":"{tool_args}"}}}}]}},"finish_reason":null}}]}}"#
        ),
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#
            .to_string(),
        "data: [DONE]".to_string(),
        "".to_string(),
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(request_write),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let final_answer = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"done"},"finish_reason":null}]}"#,
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

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let allowed_path = allowed.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
            "--yes",
            "--gateway",
            "example",
            "chat",
            "--allow-write",
            &allowed_path.to_string_lossy(),
        ]);
        cmd.write_stdin("write outside the root\n");
        cmd.assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains("denied by local policy"));
    })
    .await
    .expect("spawned scoped chat should join");

    assert!(!escaped.exists(), "--yes escaped its explicit write root");
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
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // --allow-turn-errors accepts PARTIAL success: one turn fails, one turn
    // succeeds, and the script still exits 0 with a failure summary.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("will fail"))
        .respond_with(ResponseTemplate::new(503).set_body_string("temporarily unavailable"))
        .with_priority(1)
        .mount(&server)
        .await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"still works"},"finish_reason":null}]}"#,
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
        .with_priority(10)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script_path = home.path().join("allow-errors.txt");
    fs::write(&script_path, "will fail\nthis one works\n").unwrap();
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
            .stdout(predicate::str::contains("still works"))
            .stderr(predicate::str::contains("script summary: 1 failure"));
    })
    .await
    .expect("join");
}

/// `--allow-turn-errors` accepts partial success, not total failure: a script
/// where every turn failed must still exit nonzero.
#[tokio::test]
#[serial]
async fn test_script_allow_turn_errors_all_failed_exits_nonzero() {
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
    let script_path = home.path().join("all-failed.txt");
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
            .failure()
            .code(2)
            .stderr(predicate::str::contains(
                "all 1 turn(s) or command(s) failed",
            ));
    })
    .await
    .expect("join");
}

/// F2 (cycle 6): a tool error the model recovers from within the turn (failed
/// call, then a corrected successful call, then a final answer) must COMMIT in
/// strict scripted mode — the turn is judged by its final state, not by the
/// cumulative error count.
#[tokio::test]
#[serial]
async fn test_scripted_recovered_tool_error_commits_turn() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // Round 3: after the successful read result ("median-ok") is fed back,
    // finish with the answer.
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("median-ok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "recovered and finished"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    // Round 2: the first tool call failed; retry with corrected arguments.
    let data_path_holder = TempDir::new().unwrap();
    let data_path = data_path_holder.path().join("data.txt");
    fs::write(&data_path, "median-ok").unwrap();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "retry_ok",
                        "type": "function",
                        "function": {
                            "name": "fs_read_file",
                            "arguments": serde_json::json!({"path": data_path}).to_string()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(5)
        .mount(&server)
        .await;
    // Round 1: a malformed tool call that fails locally (bad arguments).
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "first_try",
                        "type": "function",
                        "function": {"name": "fs_read_file", "arguments": "{}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("recover.script");
    fs::write(&script, "read the data file\n").unwrap();
    let home_path = home.path().to_path_buf();
    let allowed = data_path_holder.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args([
            "--gateway",
            "example",
            "--no-stream",
            "chat",
            "--allow-path",
            &allowed.to_string_lossy(),
            "--script",
            &script.to_string_lossy(),
        ]);
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("recovered and finished"))
            .stderr(predicate::str::contains(
                "1 earlier tool error(s) were recovered within the turn",
            ))
            .stderr(predicate::str::contains("strict scripted turn failed").not());
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

#[tokio::test]
#[serial]
async fn test_jsonl_strict_script_separates_local_tool_and_policy_failures() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn run_case(tool_args: String, expected_kind: &str, expected_policy_errors: u64) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(CHAT_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "local_failure_1",
                            "type": "function",
                            "function": {"name": "fs_read_file", "arguments": tool_args}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .with_priority(10)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(CHAT_PATH))
            .and(body_string_contains("\"role\":\"tool\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": "handled locally"},
                    "finish_reason": "stop"
                }]
            })))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;

        let home = TempDir::new().unwrap();
        let allowed = TempDir::new().unwrap();
        write_config(home.path(), &server.uri());
        let script = home.path().join(format!("{expected_kind}.script"));
        fs::write(&script, "exercise the tool\n").unwrap();
        let home_path = home.path().to_path_buf();
        let allowed_path = allowed.path().to_path_buf();

        let output = tokio::task::spawn_blocking(move || {
            let mut cmd = base_cmd(&home_path);
            cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
                .args([
                    "--jsonl",
                    "--no-stream",
                    "chat",
                    "--allow-path",
                    &allowed_path.to_string_lossy(),
                    "--script",
                    &script.to_string_lossy(),
                ])
                .output()
                .unwrap()
        })
        .await
        .unwrap();

        assert_eq!(output.status.code(), Some(1));
        assert!(output.stderr.is_empty());
        let events = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let error = events
            .iter()
            .find(|event| event["type"] == "error")
            .unwrap();
        assert_eq!(error["kind"], expected_kind);
        assert_eq!(error["exit_code"], 1);
        assert_eq!(error["tool_errors"], 1);
        assert_eq!(error["policy_errors"], expected_policy_errors);
        let turn = events
            .iter()
            .find(|event| event["type"] == "turn_finished")
            .unwrap();
        assert_eq!(turn["status"], "error");
        assert_eq!(turn["tool_errors"], 1);
        assert_eq!(turn["policy_errors"], expected_policy_errors);
        let summary = events.last().unwrap();
        assert_eq!(summary["type"], "final_summary");
        assert_eq!(summary["provider_failures"], 0);
        assert_eq!(summary["user_failures"], 0);
        assert_eq!(summary["tool_failures"], u64::from(expected_kind == "tool"));
        assert_eq!(
            summary["policy_failures"],
            u64::from(expected_kind == "policy")
        );
        server.verify().await;
    }

    let outside = TempDir::new().unwrap();
    let outside_file = outside.path().join("outside.txt");
    fs::write(&outside_file, "outside").unwrap();
    run_case(
        serde_json::json!({"path": outside_file}).to_string(),
        "policy",
        1,
    )
    .await;
    run_case("{}".to_string(), "tool", 0).await;
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

/// Cycle-10 G1 acceptance: a strict scripted turn that completes a file write
/// and then fails is rolled back, but the retained on-disk change is not
/// orphaned — the next turn's request carries a host-authored note naming the
/// written path (so the model can name its own edits without re-reading
/// blind), stderr says what was retained, and the end-of-run summary repeats
/// the count.
#[tokio::test]
#[serial]
async fn test_rolled_back_turn_briefs_next_turn_about_retained_file_changes() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let repo = TempDir::new().unwrap();
    let written = repo.path().join("retained.txt");
    let write_args = format!(
        r#"{{\"path\":\"{}\",\"content\":\"RETAINED-CONTENT-SENTINEL\"}}"#,
        written.to_string_lossy().replace('\\', "\\\\")
    );
    let write_round = [
        format!(
            r#"data: {{"choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":0,"id":"call_write","type":"function","function":{{"name":"fs_write_file","arguments":"{write_args}"}}}}]}},"finish_reason":null}}]}}"#
        ),
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#.to_string(),
        "data: [DONE]".to_string(),
        String::new(),
    ]
    .join("\n\n");
    let denied_round = [
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_denied","type":"function","function":{"name":"fs_read_file","arguments":"{\"path\":\"/etc/hostname\"}"}}]},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    let finish_round = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"attempt finished"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    let second_round = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"second turn done"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |request: &wiremock::Request| {
            let body = String::from_utf8_lossy(&request.body);
            let sse = if body.contains("SECOND_TURN_PROMPT") {
                second_round.clone()
            } else if body.contains("PolicyDenied") {
                finish_round.clone()
            } else if body.contains("\"role\":\"tool\"") {
                denied_round.clone()
            } else {
                write_round.clone()
            };
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse)
        })
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script_path = home.path().join("retained.txt.script");
    fs::write(&script_path, "WRITE_THEN_FAIL_PROMPT\nSECOND_TURN_PROMPT\n").unwrap();
    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let script = script_path.to_string_lossy().to_string();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.current_dir(&repo_path);
        cmd.args([
            "--gateway",
            "example",
            "chat",
            "--allow-read-write",
            &repo_path.to_string_lossy(),
            "--script",
            &script,
        ]);
        cmd.assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains("failed turn rolled back"))
            .stderr(predicate::str::contains(
                "file change(s) from the rolled-back turn remain on disk",
            ))
            .stderr(predicate::str::contains(
                "file change(s) from rolled-back turn(s) remain on disk",
            ));
    })
    .await
    .expect("join");

    // Ground truth: the write from the rolled-back turn is still on disk.
    assert_eq!(
        fs::read_to_string(&written).expect("retained write must survive the rollback"),
        "RETAINED-CONTENT-SENTINEL"
    );

    let requests = server.received_requests().await.unwrap();
    let recovery = requests
        .iter()
        .find(|request| String::from_utf8_lossy(&request.body).contains("SECOND_TURN_PROMPT"))
        .expect("second turn request");
    let body = String::from_utf8_lossy(&recovery.body);
    assert!(
        body.contains("failed turn retained file changes"),
        "next turn was not briefed about retained changes: {body}"
    );
    assert!(
        body.contains("retained.txt"),
        "the briefing must name the retained path: {body}"
    );
    assert!(
        !body.contains("WRITE_THEN_FAIL_PROMPT"),
        "rolled-back turn content leaked into the next turn: {body}"
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

/// `/diff` is host-owned evidence, not a model read: launched in a
/// subdirectory with **no** filesystem grant at all it still resolves the
/// pathspec against the repository root and renders the diff, and
/// `/permissions` reports no model read roots while naming the host-evidence
/// root it used.
#[test]
fn test_diff_from_subdirectory_resolves_filter_without_grants() {
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

    fs::write(&script, "/diff src/lib.rs\n/permissions\n/quit\n").unwrap();

    let mut ungranted = base_cmd(home.path());
    ungranted.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    ungranted.current_dir(&package);
    ungranted.args([
        "--gateway",
        "example",
        "chat",
        "--script",
        &script.to_string_lossy(),
    ]);
    ungranted
        .assert()
        .success()
        .stderr(predicate::str::contains("crates/widget/src/lib.rs"))
        .stderr(predicate::str::contains("+after"))
        .stderr(predicate::str::contains("read permission required").not())
        .stderr(predicate::str::contains("read roots   none"))
        .stderr(predicate::str::contains("host evidence"))
        .stderr(predicate::str::contains(
            "never a model read root, never a model filesystem target",
        ));

    // A read grant neither widens nor narrows the host-evidence scope.
    let mut granted = base_cmd(home.path());
    granted.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
    granted.current_dir(&package);
    granted.args([
        "--gateway",
        "example",
        "chat",
        "--allow-path",
        &repo.path().to_string_lossy(),
        "--script",
        &script.to_string_lossy(),
    ]);
    granted
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
            .code(1)
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

#[tokio::test]
#[serial]
async fn test_top_level_review_is_verified_tool_free_and_jsonl_framed() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "verified review complete"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 21, "completion_tokens": 3, "total_tokens": 24}
        })))
        .expect(2)
        .mount(&server)
        .await;

    let repo = TempDir::new().unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    fs::write(repo.path().join("review-me.rs"), "fn reviewed() {}\n").unwrap();
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());

    let mut human = base_cmd(home.path());
    human
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--gateway",
            "example",
            "--no-stream",
            "review",
            "--instructions",
            "focus on public API compatibility",
        ]);
    human
        .assert()
        .success()
        .stdout(predicate::str::contains("verified review complete"))
        .stderr(predicate::str::contains("verified target working"));

    let mut machine = base_cmd(home.path());
    let output = machine
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "--no-stream",
            "review",
            "--instructions",
            "focus on public API compatibility",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events[0]["type"], "run_started");
    assert_eq!(events[0]["mode"], "review");
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert_eq!(events.last().unwrap()["status"], "ok");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(
            body.get("tools")
                .map(serde_json::Value::is_null)
                .unwrap_or(true),
            "top-level review advertised tools: {body}"
        );
        let prompt = body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(prompt.contains("review-me.rs"));
        assert!(prompt.contains("focus on public API compatibility"));
    }
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jsonl_command_exec_reports_native_metadata_and_retains_private_artifact() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let argv = vec!["/bin/echo".to_string(), "KITSU_COMMAND_OK".to_string()];
    let tool_args = serde_json::json!({
        "argv": argv,
        "cwd": work.path().to_string_lossy(),
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "command_call_1",
                        "type": "function",
                        "function": {"name": "command_exec", "arguments": tool_args}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "command checked"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let allow_command = serde_json::to_string(&vec![
        "/bin/echo".to_string(),
        "KITSU_COMMAND_OK".to_string(),
    ])
    .unwrap();
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(work.path())
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--enable-commands",
            "--command-root",
            &work.path().to_string_lossy(),
            "--allow-command",
            &allow_command,
            "run the approved command",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let started = events
        .iter()
        .find(|event| event["type"] == "tool_started")
        .expect("native tool_started event");
    assert_eq!(started["name"], "command_exec");
    assert_eq!(started["command"]["argv"][0], "/bin/echo");
    assert_eq!(
        started["command"]["cwd"],
        work.path().to_string_lossy().as_ref()
    );
    let finished = events
        .iter()
        .find(|event| event["type"] == "tool_finished")
        .expect("native tool_finished event");
    assert_eq!(finished["command"]["termination"], "exited");
    assert_eq!(finished["command"]["exit_code"], 0);
    assert_eq!(finished["command"]["stdout_bytes"], 17);
    assert_eq!(finished["command"]["stdout_tail"], "KITSU_COMMAND_OK\\n");
    let artifact = finished["command"]["artifact"]["reference"]
        .as_str()
        .expect("host-owned artifact reference");
    assert_eq!(
        finished["command"]["artifact"]["captured_bytes"],
        finished["command"]["stdout_bytes"]
    );
    assert_eq!(finished["command"]["artifact"]["truncated"], false);
    assert!(finished["command"]["artifact"]["limit_bytes"]
        .as_u64()
        .is_some_and(|limit| limit > 0));
    assert!(
        Path::new(artifact).exists(),
        "one-shot command artifact should remain readable after process exit: {artifact}"
    );
    assert_eq!(
        finished["command"]["artifact"]["artifact_bytes"],
        fs::metadata(artifact).unwrap().len()
    );
    let artifact_text = fs::read_to_string(artifact).unwrap();
    assert!(artifact_text.contains("KITSU_COMMAND_OK"));
    assert!(artifact_text.contains("kitsu.command-output.v1"));
    assert_eq!(events.last().unwrap()["status"], "ok");
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

/// Cycle-9 finding 4, completed for `/queue`: a one-word subaction typo is
/// intercepted with a close-match suggestion and an escape hatch, instead of
/// being silently queued as a prompt.
#[test]
#[serial]
fn test_queue_subaction_typo_suggests_close_match_instead_of_queueing() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home.path().join("queue-typo.txt");
    fs::write(&script_path, "/queue lst\n").unwrap();
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
        .stderr(predicate::str::contains("did you mean '/queue list'"))
        .stderr(predicate::str::contains("/queue add lst"))
        .stderr(predicate::str::contains("1 / 8 prompts queued").not());
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
    fs::write(&script_path, "/tools\n/permissions\n/quit\n").unwrap();

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
        .stderr(predicate::str::contains("command_exec"))
        .stderr(predicate::str::contains("mandatory OS containment"));

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

#[test]
fn test_yes_is_scoped_and_global_filesystem_bypass_is_explicit() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let work = TempDir::new().expect("write root");
    let script = home.path().join("permissions.txt");
    fs::write(&script, "/permissions\n/quit\n").unwrap();
    let root = work.path().to_string_lossy().to_string();
    let script_path = script.to_string_lossy().to_string();

    let mut scoped = base_cmd(home.path());
    scoped.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "--yes",
        "chat",
        "--allow-write",
        &root,
        "--script",
        &script_path,
    ]);
    scoped
        .assert()
        .success()
        .stderr(predicate::str::contains("mode         scoped"))
        .stderr(predicate::str::contains("global filesystem bypass").not());

    let mut bypass = base_cmd(home.path());
    bypass.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "--dangerously-allow-all-filesystem",
        "chat",
        "--allow-write",
        &root,
        "--script",
        &script_path,
    ]);
    bypass
        .assert()
        .success()
        .stderr(predicate::str::contains("DANGER: global filesystem bypass"));

    let mut conflicting = base_cmd(home.path());
    conflicting.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "--yes",
        "--dangerously-allow-all-filesystem",
        "chat",
        "--allow-write",
        &root,
        "--script",
        &script_path,
    ]);
    conflicting
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_slash_grant_provenance_and_revoke_are_visible() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let work = TempDir::new().expect("combined root");
    let root = work.path().to_string_lossy();
    let script = home.path().join("revoke.txt");
    fs::write(
        &script,
        format!(
            "/revoke-read {root}\n/revoke-write {root}\n/permissions\n/allow-read-write {root}\n/permissions\n/quit\n"
        ),
    )
    .unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "chat",
        "--allow-read-write",
        &root,
        "--script",
        &script.to_string_lossy(),
    ]);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains(
            "read root revoked for this session (invocation)",
        ))
        .stderr(predicate::str::contains(
            "write root revoked for this session (invocation)",
        ))
        .stderr(predicate::str::contains("revoked invocation roots"))
        .stderr(predicate::str::contains("read/write access granted"))
        .stderr(predicate::str::contains("(session/runtime)"));
}

#[test]
fn test_global_filesystem_bypass_refuses_misleading_slash_revocation() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let work = TempDir::new().expect("granted root");
    let root = work.path().to_string_lossy();
    let script = home.path().join("dangerous-revoke.txt");
    fs::write(
        &script,
        format!("/revoke-read {root}\n/revoke-write {root}\n/quit\n"),
    )
    .unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "--dangerously-allow-all-filesystem",
        "chat",
        "--allow-read-write",
        &root,
        "--script",
        &script.to_string_lossy(),
    ]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "cannot revoke read roots while --dangerously-allow-all-filesystem is active",
        ))
        .stderr(predicate::str::contains(
            "cannot revoke write roots while --dangerously-allow-all-filesystem is active",
        ))
        .stderr(predicate::str::contains(
            "restart without the dangerous flag",
        ));
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

/// Provider response bodies are untrusted and may contain credentials or
/// terminal controls. The centralized diagnostic retains only the safe status
/// class and an actionable remedy.
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
            .stderr(predicate::str::contains("HTTP 503"))
            .stderr(predicate::str::contains("gateway is unavailable"))
            .stderr(predicate::str::contains("catalog unavailable").not())
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

#[test]
#[serial]
fn test_jsonl_script_query_results_are_structured_and_framed() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let skill_dir = home.path().join(".kitsu/skills/jsonl-demo");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: jsonl-demo\ndescription: JSONL fixture\nalways: false\n---\nbody\n",
    )
    .unwrap();
    let script = home.path().join("queries.txt");
    fs::write(
        &script,
        "/status\n/permissions\n/context\n/sessions\n/skills\n/jobs\n/model alternate-model\n/compact\n/quit\n",
    )
    .unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["--jsonl", "chat", "--script", &script.to_string_lossy()])
        .output()
        .expect("query script");
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let results = events
        .iter()
        .filter(|event| event["type"] == "command_result")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 8);
    assert_eq!(results[0]["result"]["kind"], "status");
    assert_eq!(results[0]["result"]["session"]["model"], "example-model-v1");
    assert!(results[0]["result"]["session"]["cwd"].is_string());
    assert!(results[0]["result"]["context"]["effective"].is_number());
    assert!(results[0]["result"]["conversation"]["messages"].is_number());
    assert!(results[0]["result"]["tools"]["names"].is_array());
    assert!(results[0]["result"]["text"].is_string());
    assert_eq!(results[1]["result"]["kind"], "permissions");
    assert_eq!(results[1]["result"]["mode"], "deny");
    assert!(results[1]["result"]["filesystem"]["read"]["active_invocation"].is_array());
    assert!(results[1]["result"]["filesystem"]["protected_write_roots"].is_array());
    assert!(results[1]["result"]["commands"]["exact_grants"].is_array());
    assert!(results[1]["result"]["text"].is_string());
    assert_eq!(results[2]["result"]["kind"], "context");
    assert_eq!(results[2]["result"]["model"], "example-model-v1");
    assert_eq!(results[3]["result"]["kind"], "sessions");
    assert_eq!(results[3]["result"]["count"], 0);
    assert!(results[3]["result"]["sessions"].is_array());
    assert_eq!(results[4]["result"]["kind"], "skills");
    assert_eq!(results[4]["result"]["count"], 1);
    assert_eq!(results[4]["result"]["skills"][0]["name"], "jsonl-demo");
    assert_eq!(results[4]["result"]["skills"][0]["origin"], "user");
    assert_eq!(results[5]["result"]["kind"], "jobs");
    assert_eq!(results[5]["result"]["action"], "list");
    assert_eq!(results[5]["result"]["snapshot"]["total"], 0);
    assert_eq!(results[6]["result"]["kind"], "model_switch");
    assert_eq!(results[6]["result"]["previous_model"], "example-model-v1");
    assert_eq!(results[6]["result"]["model"], "alternate-model");
    assert_eq!(results[6]["result"]["changed"], true);
    assert_eq!(results[6]["result"]["catalog_validated"], false);
    assert!(results[6]["result"]["effective_context_window"].is_number());
    assert_eq!(results[7]["result"]["kind"], "compaction");
    assert!(results[7]["result"]["changed"].is_boolean());
    assert!(results[7]["result"]["tokens_before"].is_number());
    assert!(results[7]["result"]["tokens_after"].is_number());
    assert!(results[7]["result"]["usage"].is_object());
    let started = events
        .iter()
        .filter(|event| event["type"] == "command_started")
        .count();
    let finished = events
        .iter()
        .filter(|event| event["type"] == "command_finished")
        .count();
    assert_eq!(started, 9);
    assert_eq!(finished, started, "every slash frame must close");
}

#[test]
#[serial]
fn test_jsonl_profiles_and_reasoning_controls_are_structured() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("profiles.txt");
    fs::write(&script, "/profiles\n/reasoning high\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["--jsonl", "chat", "--script", &script.to_string_lossy()])
        .output()
        .expect("profile query script");
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let results = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["type"] == "command_result")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["result"]["kind"], "agent_profiles");
    assert_eq!(results[0]["result"]["count"], 2);
    let names = results[0]["result"]["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|profile| profile["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(names.contains(&"reasoning"));
    assert!(names.contains(&"reviewer"));
    assert_eq!(results[1]["result"]["kind"], "reasoning");
    assert_eq!(results[1]["result"]["changed"], true);
    assert_eq!(results[1]["result"]["effort"], "high");
}

#[tokio::test]
#[serial]
async fn test_scripted_exit_surfaces_terminal_background_results_in_human_and_jsonl() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(
            "\"content\":\"background audit task\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "BACKGROUND_RESULT_SENTINEL"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(200))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "foreground complete"},
                        "finish_reason": "stop"
                    }]
                })),
        )
        .with_priority(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "background-call-1",
                        "type": "function",
                        "function": {
                            "name": "spawn_agent_bg",
                            "arguments": "{\"task\":\"background audit task\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("background-result.script");
    fs::write(&script, "start detached work\n").unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().into_owned();
    let (human, jsonl) = tokio::task::spawn_blocking(move || {
        let run = |jsonl: bool| {
            let mut cmd = base_cmd(&home_path);
            cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).arg("--no-stream");
            if jsonl {
                cmd.arg("--jsonl");
            }
            cmd.args([
                "--gateway",
                "example",
                "chat",
                "--no-session",
                "--allow-path",
                &home_path.to_string_lossy(),
                "--script",
                &script_path,
            ]);
            cmd.output().unwrap()
        };
        (run(false), run(true))
    })
    .await
    .unwrap();

    assert!(
        human.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&human.stderr),
        String::from_utf8_lossy(&human.stdout)
    );
    assert!(String::from_utf8_lossy(&human.stdout).contains("foreground complete"));
    let human_stderr = String::from_utf8_lossy(&human.stderr);
    assert!(human_stderr.contains("background job #1 result"));
    assert!(human_stderr.contains("BACKGROUND_RESULT_SENTINEL"));

    assert!(
        jsonl.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&jsonl.stderr),
        String::from_utf8_lossy(&jsonl.stdout)
    );
    assert!(jsonl.stderr.is_empty());
    let events = String::from_utf8(jsonl.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let background_index = events
        .iter()
        .position(|event| event["type"] == "background_result")
        .expect("terminal background result event");
    let summary_index = events
        .iter()
        .position(|event| event["type"] == "final_summary")
        .expect("final summary");
    assert!(background_index < summary_index);
    assert_eq!(events[background_index]["status"], "done");
    assert_eq!(
        events[background_index]["result"],
        "BACKGROUND_RESULT_SENTINEL"
    );
    assert_eq!(events[background_index]["joined"], true);
    assert_eq!(events[summary_index]["status"], "ok");
    assert_eq!(events[summary_index]["failures"], 0);
    server.verify().await;
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_durable_script_save_failure_finalizes_earlier_background_work_before_summary() {
    use std::os::unix::fs::PermissionsExt as _;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(
            "\"content\":\"durable background task\"",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(40))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "DURABLE_BACKGROUND_SENTINEL"},
                        "finish_reason": "stop"
                    }]
                })),
        )
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(200))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "foreground complete"},
                        "finish_reason": "stop"
                    }]
                })),
        )
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let sessions_dir = home.path().join(".kitsu/sessions");
    let chmod_argv = vec![
        "/bin/chmod".to_string(),
        "0500".to_string(),
        sessions_dir.to_string_lossy().into_owned(),
    ];
    let spawn_args = serde_json::json!({"task": "durable background task"}).to_string();
    let command_args = serde_json::json!({
        "argv": chmod_argv,
        "cwd": home.path(),
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "background-before-save",
                            "type": "function",
                            "function": {
                                "name": "spawn_agent_bg",
                                "arguments": spawn_args
                            }
                        },
                        {
                            "id": "block-session-save",
                            "type": "function",
                            "function": {
                                "name": "command_exec",
                                "arguments": command_args
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let script = home.path().join("durable-save-failure.script");
    fs::write(&script, "start work before the durable save\n").unwrap();
    let allow_command = serde_json::to_string(&vec![
        "/bin/chmod".to_string(),
        "0500".to_string(),
        sessions_dir.to_string_lossy().into_owned(),
    ])
    .unwrap();
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(home.path())
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "--no-stream",
            "chat",
            "--session",
            "--allow-turn-errors",
            "--enable-commands",
            "--command-root",
            &home.path().to_string_lossy(),
            "--allow-command",
            &allow_command,
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("durable scripted run");
    // Let TempDir clean up even though the child deliberately removed write
    // permission from the session namespace.
    if sessions_dir.exists() {
        fs::set_permissions(&sessions_dir, fs::Permissions::from_mode(0o700)).unwrap();
    }

    assert_eq!(
        output.status.code(),
        Some(1),
        "durability remains fatal even with --allow-turn-errors\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let background_index = events
        .iter()
        .position(|event| event["type"] == "background_result")
        .expect("unobserved background result");
    let error_index = events
        .iter()
        .position(|event| {
            event["type"] == "error"
                && event["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("session autosave failed"))
        })
        .expect("durable save error");
    let summary_index = events
        .iter()
        .position(|event| event["type"] == "final_summary")
        .expect("final summary");
    assert!(background_index < error_index);
    assert!(error_index < summary_index);
    assert_eq!(summary_index, events.len() - 1, "summary must be last");
    assert_eq!(events[background_index]["status"], "done");
    assert_eq!(
        events[background_index]["result"],
        "DURABLE_BACKGROUND_SENTINEL"
    );
    assert_eq!(events[background_index]["joined"], true);
    assert_eq!(events[summary_index]["status"], "error");
    assert_eq!(events[summary_index]["failures"], 1);
    assert_eq!(events[summary_index]["provider_failures"], 0);
    assert_eq!(events[summary_index]["user_failures"], 1);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_jsonl_run_started_reports_resolved_session_and_model() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"saved answer"},"finish_reason":"stop"}]}"#,
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
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let create_script = home.path().join("session-create.txt");
    let resume_script = home.path().join("session-query.txt");
    fs::write(&create_script, "create real durable history\n").unwrap();
    fs::write(&resume_script, "/status\n/quit\n").unwrap();
    let home_path = home.path().to_path_buf();

    let (created, resumed) = tokio::task::spawn_blocking(move || {
        let run = |extra: &[&str], script: &Path| {
            let mut cmd = base_cmd(&home_path);
            let mut args = vec!["--jsonl", "chat"];
            args.extend_from_slice(extra);
            args.extend(["--script", script.to_str().unwrap()]);
            let output = cmd
                .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
                .args(args)
                .output()
                .expect("session query");
            assert!(
                output.status.success(),
                "stderr: {}\nstdout: {}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                output.stderr.is_empty(),
                "JSONL must suppress durable/resume/project-context notices: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .find(|event| event["type"] == "run_started")
                .expect("run_started")
        };

        let created = run(&["--session", "--model", "saved-model"], &create_script);
        let id = created["session_id"]
            .as_str()
            .expect("new durable session id")
            .to_string();
        let resumed = run(&["--resume", &id], &resume_script);
        (created, resumed)
    })
    .await
    .unwrap();

    let id = created["session_id"].as_str().unwrap();
    assert_eq!(created["model"], "saved-model");
    assert_eq!(created["resumed"], false);
    assert_eq!(created["durable_session"], true);
    assert_eq!(resumed["session_id"], id);
    assert_eq!(resumed["model"], "saved-model");
    assert_eq!(resumed["resumed"], true);
    assert_eq!(resumed["durable_session"], true);
    server.verify().await;
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

// --- deadline expiry preserves delivered partial output (cycle 6) ------------

/// SSE server that streams one real fragment and then stalls until the client
/// gives up, so the inference deadline expires mid-answer. Serves any number
/// of connections; abort the returned handle when done.
async fn spawn_stalling_sse_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 2048];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let Ok(read) = socket.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
                    )
                    .await;
                let fragment = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial answer under deadline\"},\"finish_reason\":null}]}\n\n";
                let _ = socket
                    .write_all(format!("{:x}\r\n", fragment.len()).as_bytes())
                    .await;
                let _ = socket.write_all(fragment).await;
                let _ = socket.write_all(b"\r\n").await;
                let _ = socket.flush().await;
                // Stall: never finish the body. The client's deadline fires.
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            });
        }
    });
    (address, server)
}

/// Human mode: a deadline that expires mid-stream keeps the delivered partial
/// answer, closes it with an in-band truncation marker plus a trailing newline
/// (so stderr never fuses onto the stream), and reports a partial notice
/// instead of DISCARDED scare text. The exit code stays 2.
#[tokio::test]
#[serial]
async fn test_ask_timeout_preserves_partial_streamed_answer() {
    let (address, server) = spawn_stalling_sse_server().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &format!("http://{address}"));
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--timeout-secs", "2", "ask", "long question"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    server.abort();
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stdout.contains("partial answer under deadline"),
        "delivered partial text must be kept: {stdout:?}"
    );
    assert!(
        stdout.contains("[stopped: deadline expired - answer is partial]"),
        "stdout must carry an in-band truncation marker: {stdout:?}"
    );
    assert!(
        stdout.ends_with('\n'),
        "the partial stream must be newline-terminated: {stdout:?}"
    );
    assert!(
        stderr.contains("timed out after 2s - the answer above is partial"),
        "stderr must explain the partial outcome: {stderr:?}"
    );
    assert!(
        !stderr.contains("DISCARDED"),
        "delivered output must not be labeled DISCARDED: {stderr:?}"
    );
}

/// JSONL mode: the same expiry closes the provisional-output transaction as
/// `committed_partial` with a final `assistant` payload matching the delta
/// concatenation, while the turn still fails (error event, exit 2).
#[tokio::test]
#[serial]
async fn test_jsonl_ask_timeout_emits_committed_partial_disposition() {
    let (address, server) = spawn_stalling_sse_server().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &format!("http://{address}"));
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--jsonl", "--timeout-secs", "2", "ask", "long question"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    server.abort();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let deltas: String = events
        .iter()
        .filter(|event| event["type"] == "assistant_delta")
        .map(|event| event["text"].as_str().unwrap())
        .collect();
    let assistant = events
        .iter()
        .position(|event| event["type"] == "assistant")
        .expect("partial assistant payload must be emitted");
    assert_eq!(events[assistant]["text"].as_str().unwrap(), deltas);
    assert!(deltas.contains("partial answer under deadline"));
    assert!(deltas.ends_with("[stopped: deadline expired - answer is partial]"));
    let disposition = events
        .iter()
        .position(|event| event["type"] == "assistant_disposition")
        .expect("disposition must close the transaction");
    assert_eq!(events[disposition]["status"], "committed_partial");
    assert!(events[disposition]["reason"]
        .as_str()
        .unwrap()
        .contains("timed out after 2s"));
    let error = events
        .iter()
        .position(|event| event["type"] == "error")
        .expect("the turn still fails");
    assert_eq!(events[error]["exit_code"], 2);
    assert!(assistant < disposition && disposition < error);
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert_eq!(events.last().unwrap()["status"], "error");
}

/// The one-shot piped path (here: top-level `review`, which streams without
/// tools) gets the same honest partial outcome on deadline expiry.
#[tokio::test]
#[serial]
async fn test_review_timeout_preserves_partial_streamed_answer() {
    let (address, server) = spawn_stalling_sse_server().await;
    let repo = TempDir::new().unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    fs::write(repo.path().join("review-me.rs"), "fn reviewed() {}\n").unwrap();
    let home = TempDir::new().unwrap();
    write_config(home.path(), &format!("http://{address}"));
    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .current_dir(&repo_path)
            .args([
                "--gateway",
                "example",
                "--timeout-secs",
                "2",
                "review",
                "--instructions",
                "review the working tree",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    server.abort();
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("partial answer under deadline"));
    assert!(stdout.contains("[stopped: deadline expired - answer is partial]"));
    assert!(stderr.contains("timed out after 2s - the answer above is partial"));
    assert!(!stderr.contains("DISCARDED"));
}

// --- context-window runtime cross-check (cycle 6) ----------------------------

fn window_mismatch_sse() -> String {
    [
        r#"data: {"choices":[{"index":0,"delta":{"content":"generic answer"},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":4096,"completion_tokens":2,"total_tokens":4098}}"#,
        "data: [DONE]",
        "",
    ]
    .join("\n\n")
}

/// When the provider reports accepting far fewer prompt tokens than the sent
/// estimate (the shape of a server clamping to a smaller real window), human
/// mode warns once on stderr without failing the turn.
#[tokio::test]
#[serial]
async fn test_context_window_mismatch_warns_on_stderr() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(window_mismatch_sse()),
        )
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let big_prompt = "review this attachment ".repeat(1000); // ~23k chars ≈ 5.7k tokens
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["ask", &big_prompt])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("context window mismatch suspected"),
        "stderr must warn about the truncating server window: {stderr:?}"
    );
    assert!(stderr.contains("accepted 4096"));
}

/// JSONL mode surfaces the same evidence as a typed event.
#[tokio::test]
#[serial]
async fn test_jsonl_context_window_mismatch_emits_typed_event() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(window_mismatch_sse()),
        )
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let big_prompt = "review this attachment ".repeat(1000);
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--jsonl", "ask", &big_prompt])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let mismatch = events
        .iter()
        .find(|event| event["type"] == "context_window_mismatch")
        .unwrap_or_else(|| panic!("typed mismatch event missing: {events:#?}"));
    assert_eq!(mismatch["reported_prompt_tokens"], 4096);
    assert_eq!(mismatch["resolved_window"], 8192);
    assert!(mismatch["estimated_sent_tokens"].as_u64().unwrap() > 4096);
    assert_eq!(events.last().unwrap()["status"], "ok");
}

/// A scripted `/context` after the mismatch turn carries the evidence in its
/// provenance payload, and the stderr warning fires exactly once.
#[tokio::test]
#[serial]
async fn test_context_command_surfaces_window_runtime_evidence() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(window_mismatch_sse()),
        )
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("mismatch.script");
    let big_prompt = "review this attachment ".repeat(1000).replace('\n', " ");
    fs::write(&script, format!("{big_prompt}\n/context\n")).unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().to_string();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--jsonl", "chat", "--script", &script_path])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "context_window_mismatch")
            .count(),
        1,
        "the mismatch warning must fire exactly once: {events:#?}"
    );
    let context = events
        .iter()
        .find(|event| event["type"] == "command_result" && event["result"]["kind"] == "context")
        .expect("scripted /context result");
    let evidence = &context["result"]["runtime_evidence"];
    assert_eq!(evidence["reported_prompt_tokens"], 4096);
    assert_eq!(evidence["resolved_window"], 8192);
}

/// Gap 1 (cycle 6): the system prompt sent to the provider must tell the model
/// its working directory and every granted root with its authority, and steer
/// it to explore instead of asking the user for paths.
#[tokio::test]
#[serial]
async fn test_piped_chat_system_prompt_names_cwd_and_granted_roots() {
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

    let home = TempDir::new().unwrap();
    let workdir = TempDir::new().unwrap();
    let granted = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let workdir_path = workdir.path().to_path_buf();
    let granted_path = granted.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .current_dir(&workdir_path)
            .args([
                "--gateway",
                "example",
                "chat",
                "--allow-path",
                &granted_path.to_string_lossy(),
                "Explain this codebase",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(0));

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("Current working directory:"),
        "system prompt must name the cwd: {system:?}"
    );
    // Paths may be canonicalized for display; assert on the directory name.
    let granted_name = granted.path().file_name().unwrap().to_string_lossy();
    assert!(
        system.contains(&format!("{granted_name} (read-only)")),
        "system prompt must list the granted root with its authority: {system:?}"
    );
    assert!(
        system.contains("explore the granted roots"),
        "system prompt must steer the model to explore: {system:?}"
    );
}

// --- agent deck: profile spawns, non-interactive joins, broken pipes ---------

/// Cycle-7 F1: selecting a built-in profile must not inject a `reasoning_effort`
/// field. The CLI flag is "omitted by default so non-reasoning endpoints receive
/// no extra request field"; a profile spawn that ignored that rule made every
/// profile 400 on the default local gateway while the model reported success.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_profile_background_spawn_sends_no_reasoning_effort_field() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"content\":\"profile audit task\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "PROFILE_RESULT_SENTINEL"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "foreground complete"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "profile-call-1",
                        "type": "function",
                        "function": {
                            "name": "spawn_agent_bg",
                            "arguments": "{\"task\":\"profile audit task\",\"profile\":\"reviewer\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("profile-spawn.script");
    fs::write(&script, "delegate an audit\n").unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().into_owned();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).arg("--no-stream");
        cmd.args([
            "--gateway",
            "example",
            "chat",
            "--no-session",
            "--allow-path",
            &home_path.to_string_lossy(),
            "--script",
            &script_path,
        ]);
        cmd.output().unwrap()
    })
    .await
    .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("PROFILE_RESULT_SENTINEL"),
        "stderr: {stderr}"
    );

    let requests = server.received_requests().await.expect("recorded requests");
    assert!(!requests.is_empty());
    for request in &requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(
            body.get("reasoning_effort").is_none(),
            "a profile that requests no effort must not add the field: {body}"
        );
    }
}

/// Cycle-7 F2b / git-workflow gap 2: a script that ends while a background agent
/// is healthy joins it (the `/quit wait` semantics), reports its real result, and
/// exits 0 — instead of cancelling the work and blaming a turn that committed.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_script_exit_joins_live_background_agents_and_never_claims_a_rollback() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"content\":\"slow detached task\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(900))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "DETACHED_RESULT_SENTINEL"},
                        "finish_reason": "stop"
                    }]
                })),
        )
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "foreground complete"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "detached-call-1",
                        "type": "function",
                        "function": {
                            "name": "spawn_agent_bg",
                            "arguments": "{\"task\":\"slow detached task\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("join-at-exit.script");
    fs::write(&script, "start detached work\n").unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().into_owned();
    let (human, jsonl) = tokio::task::spawn_blocking(move || {
        let run = |jsonl: bool| {
            let mut cmd = base_cmd(&home_path);
            cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).arg("--no-stream");
            if jsonl {
                cmd.arg("--jsonl");
            }
            cmd.args([
                "--gateway",
                "example",
                "chat",
                "--no-session",
                "--allow-path",
                &home_path.to_string_lossy(),
                "--script",
                &script_path,
            ]);
            cmd.output().unwrap()
        };
        (run(false), run(true))
    })
    .await
    .unwrap();

    let stderr = String::from_utf8_lossy(&human.stderr).into_owned();
    assert!(human.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("joining them"), "stderr: {stderr}");
    assert!(
        stderr.contains("joined 1 background agent(s) before exit"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("DETACHED_RESULT_SENTINEL"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("rolled back"),
        "the turn committed; nothing was rolled back: {stderr}"
    );
    assert!(!stderr.contains("cancelled"), "stderr: {stderr}");

    assert!(
        jsonl.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&jsonl.stderr)
    );
    let events = String::from_utf8(jsonl.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let background = events
        .iter()
        .find(|event| event["type"] == "background_result")
        .expect("background result event");
    assert_eq!(background["status"], "done");
    assert_eq!(background["joined"], true);
    assert_eq!(background["result"], "DETACHED_RESULT_SENTINEL");
    let summary = events
        .iter()
        .find(|event| event["type"] == "final_summary")
        .expect("final summary");
    assert_eq!(summary["status"], "ok");
    assert_eq!(summary["failures"], 0);
    server.verify().await;
}

/// When the bounded join really is exceeded, the cancellation names the policy
/// that caused it. It must never reuse the rolled-back-turn wording.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_script_exit_cancellation_names_shutdown_policy_not_a_rollback() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(
            "\"content\":\"endless detached task\"",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "never observed"},
                        "finish_reason": "stop"
                    }]
                })),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "foreground complete"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "endless-call-1",
                        "type": "function",
                        "function": {
                            "name": "spawn_agent_bg",
                            "arguments": "{\"task\":\"endless detached task\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("cancel-at-exit.script");
    fs::write(&script, "start endless work\n").unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().into_owned();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).arg("--no-stream");
        cmd.args([
            "--gateway",
            "example",
            "--agent-join-timeout",
            "0",
            "chat",
            "--no-session",
            "--allow-path",
            &home_path.to_string_lossy(),
            "--script",
            &script_path,
        ]);
        cmd.output().unwrap()
    })
    .await
    .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("cancelled by shutdown policy"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("parent turn was rolled back"),
        "the turn committed; the cancellation must not blame a rollback: {stderr}"
    );
    assert!(
        !stderr.contains("rolled-back turn"),
        "the turn committed; the cancellation must not blame a rollback: {stderr}"
    );
}

/// Git-workflow gap 3: `kitsu <cmd> | head -1` used to exit 101 with a Rust
/// panic message. A reader that goes away first is not a crash.
#[cfg(unix)]
#[test]
#[serial]
fn test_closed_output_pipe_exits_cleanly_without_a_panic() {
    use std::process::Stdio;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    for args in [vec!["--help"], vec!["sessions"], vec!["sessions", "--json"]] {
        let mut command = std::process::Command::new(
            assert_cmd::cargo::cargo_bin("kitsu")
                .to_string_lossy()
                .into_owned(),
        );
        command
            .env("HOME", home.path())
            .env("KITSU_NO_PROJECT_CONTEXT", "1")
            .env_remove("ANTHROPIC_TOKEN_EXPIRES")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn kitsu");
        // Close the read end before the child writes: exactly what `| head -1`
        // does once it has what it wants.
        drop(child.stdout.take());
        let mut stderr = String::new();
        if let Some(mut handle) = child.stderr.take() {
            use std::io::Read as _;
            let _ = handle.read_to_string(&mut stderr);
        }
        let status = child.wait().expect("wait for kitsu");
        assert_ne!(
            status.code(),
            Some(101),
            "{args:?} panicked on a closed stdout: {stderr}"
        );
        assert!(
            !stderr.contains("panicked at"),
            "{args:?} printed a panic report: {stderr}"
        );
        assert_eq!(status.code(), Some(0), "{args:?} stderr: {stderr}");
    }
}

// --- --allow-command explicit cwd (cycle 7 item 1) --------------------------

/// The object form binds a pregrant to a working directory the user names, so
/// automation launched outside a `--command-root` no longer has to `cd` first.
/// The grant stays exact argv *and* exact cwd: a sibling directory inside the
/// same root is not authorized by it.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_allow_command_object_form_binds_explicit_cwd_exactly() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let proj = TempDir::new().unwrap();
    let launch = TempDir::new().unwrap();
    let sub = proj.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    let argv = vec!["/bin/echo".to_string(), "KITSU_EXPLICIT_CWD".to_string()];

    let run = |cwd: std::path::PathBuf| {
        let argv = argv.clone();
        let proj_root = proj.path().to_path_buf();
        let launch_dir = launch.path().to_path_buf();
        async move {
            let server = MockServer::start().await;
            let tool_args = serde_json::json!({"argv": argv, "cwd": cwd}).to_string();
            Mock::given(method("POST"))
                .and(path(CHAT_PATH))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "command_call_1",
                                "type": "function",
                                "function": {"name": "command_exec", "arguments": tool_args}
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                })))
                .with_priority(10)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(CHAT_PATH))
                .and(body_string_contains("\"role\":\"tool\""))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "done"},
                        "finish_reason": "stop"
                    }]
                })))
                .with_priority(1)
                .mount(&server)
                .await;

            let home = TempDir::new().unwrap();
            write_config(home.path(), &server.uri());
            let pregrant = serde_json::json!({"argv": argv, "cwd": proj_root}).to_string();
            let mut cmd = base_cmd(home.path());
            let output = cmd
                .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
                .current_dir(&launch_dir)
                .args([
                    "--gateway",
                    "example",
                    "--jsonl",
                    "--no-stream",
                    "chat",
                    "--no-session",
                    "--command-root",
                    &proj_root.to_string_lossy(),
                    "--allow-command",
                    &pregrant,
                    "run the pre-approved command",
                ])
                .output()
                .unwrap();
            // `home` must outlive the child process.
            drop(home);
            output
        }
    };

    // Granted cwd: startup succeeds from a launch directory outside the root and
    // the command runs unprompted.
    let granted = run(proj.path().to_path_buf()).await;
    assert!(
        granted.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&granted.stderr),
        String::from_utf8_lossy(&granted.stdout)
    );
    let events = String::from_utf8(granted.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let finished = events
        .iter()
        .find(|event| event["type"] == "tool_finished")
        .expect("tool_finished event");
    assert_eq!(finished["command"]["exit_code"], 0);
    assert!(finished["command"]["stdout_tail"]
        .as_str()
        .unwrap()
        .contains("KITSU_EXPLICIT_CWD"));

    // A subdirectory of the same command root is a different cwd: still denied.
    let sibling = run(sub).await;
    let sibling_events = String::from_utf8(sibling.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        !sibling_events
            .iter()
            .any(|event| event["type"] == "tool_finished"
                && event["command"]["exit_code"] == serde_json::json!(0)),
        "an explicit-cwd grant must not authorize a sibling directory: {sibling_events:?}"
    );
}

/// The bare array form keeps meaning "the launch directory", and `/permissions`
/// says which of the two the user actually wrote.
#[test]
#[serial]
fn test_allow_command_permissions_report_cwd_provenance() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let proj = TempDir::new().expect("command root");
    let script = home.path().join("permissions.txt");
    fs::write(&script, "/permissions\n/quit\n").unwrap();

    let mut legacy = base_cmd(home.path());
    legacy
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(proj.path())
        .args([
            "chat",
            "--allow-command",
            r#"["/bin/echo","hi"]"#,
            "--script",
            &script.to_string_lossy(),
        ]);
    legacy
        .assert()
        .success()
        .stderr(predicate::str::contains("launch directory"))
        .stderr(predicate::str::contains("explicit cwd").not());

    let explicit_spec =
        serde_json::json!({"argv": ["/bin/echo", "hi"], "cwd": proj.path()}).to_string();
    let mut explicit = base_cmd(home.path());
    explicit
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(proj.path())
        .args([
            "chat",
            "--command-root",
            &proj.path().to_string_lossy(),
            "--allow-command",
            &explicit_spec,
            "--script",
            &script.to_string_lossy(),
        ]);
    explicit
        .assert()
        .success()
        .stderr(predicate::str::contains("explicit cwd"));
}

/// `/permissions --jsonl` carries the same provenance as an additive key.
#[test]
#[serial]
fn test_permissions_payload_reports_cwd_source() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let proj = TempDir::new().expect("command root");
    let script = home.path().join("permissions.txt");
    fs::write(&script, "/permissions\n/quit\n").unwrap();
    let spec = serde_json::json!({"argv": ["/bin/echo", "hi"], "cwd": proj.path()}).to_string();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(proj.path())
        .args([
            "--jsonl",
            "chat",
            "--command-root",
            &proj.path().to_string_lossy(),
            "--allow-command",
            &spec,
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("permissions script");
    assert!(output.status.success());
    let permissions = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["result"]["kind"] == "permissions")
        .expect("permissions payload");
    assert_eq!(
        permissions["result"]["commands"]["exact_grants"][0]["cwd_source"],
        "explicit"
    );
}

/// --dangerously-skip-permissions is the unattended umbrella: filesystem
/// bypass plus command auto-approval, disclosed loudly at startup and in
/// /permissions — and `/permissions protect <path>` layers an in-session
/// guard the bypass cannot cross.
#[test]
#[serial]
fn test_dangerously_skip_permissions_discloses_and_protect_guards() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let proj = TempDir::new().expect("command root");
    let guarded = proj.path().join("secrets");
    fs::create_dir(&guarded).unwrap();
    let script = home.path().join("skip.txt");
    fs::write(
        &script,
        format!(
            "/permissions protect {}\n/permissions\n/quit\n",
            guarded.display()
        ),
    )
    .unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(proj.path())
        .args([
            "--jsonl",
            "--dangerously-skip-permissions",
            "chat",
            "--command-root",
            &proj.path().to_string_lossy(),
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("skip-permissions script");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DANGEROUSLY SKIPPING PERMISSIONS"),
        "startup warning missing: {stderr}"
    );
    let permissions = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .rfind(|event| event["result"]["kind"] == "permissions")
        .expect("permissions payload");
    assert_eq!(
        permissions["result"]["commands"]["mode"],
        "DANGEROUS auto-allow (no prompts)"
    );
    assert_eq!(
        permissions["result"]["mode"],
        "DANGER: global filesystem bypass"
    );
    assert_eq!(permissions["result"]["dangerous_global_bypass"], true);
    let protected_writes = permissions["result"]["filesystem"]["protected_write_roots"]
        .as_array()
        .expect("protected_write_roots");
    assert!(
        protected_writes
            .iter()
            .any(|path| path.as_str().is_some_and(|path| path.ends_with("secrets"))),
        "protected path missing from payload: {protected_writes:?}"
    );
    let text = permissions["result"]["text"].as_str().expect("text");
    assert!(
        text.contains("secrets"),
        "protected path missing from disclosure: {text}"
    );
}

/// When the legacy form is unsatisfiable because the launch directory sits
/// outside the roots, the startup error names the object syntax that fixes it.
#[test]
#[serial]
fn test_allow_command_launch_cwd_outside_roots_names_object_syntax() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let proj = TempDir::new().expect("command root");
    let launch = TempDir::new().expect("launch dir");
    let root = proj.path().canonicalize().unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(launch.path())
        .args([
            "chat",
            "--command-root",
            &proj.path().to_string_lossy(),
            "--allow-command",
            r#"["/bin/echo","hi"]"#,
            "--no-session",
            "hello",
        ]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("\"cwd\""))
        .stderr(predicate::str::contains(root.to_string_lossy().to_string()));
}

/// Ambiguous spellings fail closed at startup rather than silently degrading to
/// a launch-directory grant.
#[test]
#[serial]
fn test_allow_command_rejects_unknown_fields_and_relative_cwd() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let proj = TempDir::new().expect("command root");

    for (spec, needle) in [
        (
            r#"{"argv":["/bin/echo"],"cd":"/tmp"}"#,
            "invalid --allow-command value",
        ),
        (
            r#"{"argv":["/bin/echo"],"cwd":"relative"}"#,
            "must be an absolute path",
        ),
    ] {
        let mut cmd = base_cmd(home.path());
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .current_dir(proj.path())
            .args([
                "chat",
                "--command-root",
                &proj.path().to_string_lossy(),
                "--allow-command",
                spec,
                "--no-session",
                "hello",
            ]);
        cmd.assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains(needle));
    }
}

// --- pregranted commands disclosed to the model (cycle 8 item 2) -----------

/// Cycle-8 audit gap 3: the exact `--allow-command` pregrant must be named
/// verbatim (argv + cwd) in the system prompt sent to the provider, not just
/// surfaced through `/permissions`/JSONL — otherwise a tool-capable model can
/// only guess an invocation. This never exercises the tool call itself: the
/// disclosure must already be present in the very first request.
#[tokio::test]
#[serial]
async fn test_system_prompt_discloses_the_exact_pregranted_command() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let proj = TempDir::new().unwrap();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "done"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let pregrant = serde_json::json!({"argv": ["pytest", "-v"], "cwd": proj.path()}).to_string();
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "--no-stream",
            "chat",
            "--no-session",
            "--command-root",
            &proj.path().to_string_lossy(),
            "--allow-command",
            &pregrant,
            "say hi",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    let system = messages
        .iter()
        .find(|message| message["role"] == "system")
        .expect("request should carry a system message");
    let content = system["content"].as_str().unwrap();
    assert!(content.contains("Pre-approved commands"), "{content}");
    assert!(content.contains("pytest"), "{content}");
    assert!(
        content.contains(&proj.path().to_string_lossy().to_string()),
        "{content}"
    );
    server.verify().await;
}

// --- containment environment pass-through (cycle 8 item 1) ------------------

/// Cycle-8 audit gap 1: a bare `--command-env NAME` pass-through of a
/// secret-shaped name must be refused at startup, before any provider
/// request is made — the fixed non-secret containment environment stays
/// deny-by-default unless the caller uses the explicit `NAME=value` form.
#[tokio::test]
#[serial]
async fn test_command_env_bare_passthrough_of_secret_shaped_name_is_refused_at_startup() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:0");
    let proj = TempDir::new().unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .env("KITSU_TEST_CLI_API_KEY", "should-never-leak")
        .args([
            "chat",
            "--no-session",
            "--command-root",
            &proj.path().to_string_lossy(),
            "--command-env",
            "KITSU_TEST_CLI_API_KEY",
            "hello",
        ]);
    cmd.assert().failure().code(1).stderr(
        predicate::str::contains("KITSU_TEST_CLI_API_KEY")
            .and(predicate::str::contains("secret-bearing")),
    );
}

// --- host-owned workspace evidence (cycle 7 item 2) -------------------------

/// Host evidence is not model authority. `/diff` succeeds with no grants, and
/// the same session still advertises no filesystem tools and reports no read
/// roots — the diff bytes reached the terminal, never the model's reach.
#[test]
#[serial]
fn test_diff_authority_does_not_make_files_model_readable() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let repo = TempDir::new().unwrap();
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
    fs::write(repo.path().join("secret.txt"), "before\n").unwrap();
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
    fs::write(repo.path().join("secret.txt"), "after\n").unwrap();
    let script = repo.path().join("script.txt");
    fs::write(&script, "/diff\n/tools\n/permissions\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &script.to_string_lossy(),
        ]);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains("+after"))
        .stderr(predicate::str::contains("fs_read_file").not())
        .stderr(predicate::str::contains("read roots   none"))
        .stderr(predicate::str::contains("host evidence"));
}

/// `/diff --jsonl` carries the host bytes and says plainly that nothing was
/// disclosed to the model, and the stream contains no grant of any kind.
#[test]
#[serial]
fn test_diff_jsonl_reports_host_only_evidence() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let repo = TempDir::new().unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    fs::write(repo.path().join("new.txt"), "fresh\n").unwrap();
    let script = repo.path().join("script.txt");
    fs::write(&script, "/diff\n/permissions\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "chat",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let diffs = events
        .iter()
        .filter(|event| event["result"]["kind"] == "diff")
        .collect::<Vec<_>>();
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0]["result"]["model_disclosed"], false);
    assert!(diffs[0]["result"]["evidence_bytes"].as_u64().unwrap() > 0);
    assert!(diffs[0]["result"]["diff"]
        .as_str()
        .unwrap()
        .contains("fresh"));

    let permissions = events
        .iter()
        .find(|event| event["result"]["kind"] == "permissions")
        .expect("permissions payload");
    assert_eq!(
        permissions["result"]["host_evidence"]["model_read_root"],
        false
    );
    // The workspace_diff tool is registered in this worktree, so prompt
    // disclosure can also be initiated by an announced model tool call.
    assert_eq!(
        permissions["result"]["host_evidence"]["prompt_disclosure"],
        "user_command_or_workspace_diff_tool"
    );
    assert!(permissions["result"]["host_evidence"]["consumers"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("workspace_diff")));
    assert!(permissions["result"]["host_evidence"]["repository_root"].is_string());
    assert!(
        permissions["result"]["filesystem"]["read"]["active_invocation"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// A workspace that lives inside Kitsu's own private state is refused outright
/// rather than disclosed as "workspace" evidence.
#[test]
#[serial]
fn test_evidence_root_inside_private_state_is_refused() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let inside = home.path().join(".kitsu/inside-repo");
    fs::create_dir_all(&inside).unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(&inside)
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    fs::write(inside.join("note.txt"), "private\n").unwrap();
    let script = home.path().join("script.txt");
    fs::write(&script, "/diff\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(&inside)
        .args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &script.to_string_lossy(),
        ]);
    cmd.assert().failure().stderr(predicate::str::contains(
        "refusing to gather workspace evidence from inside Kitsu-private state",
    ));
}

/// `kitsu review` no longer fabricates a repository read root for itself. It
/// discloses exactly one bounded diff and says so.
#[tokio::test]
#[serial]
async fn test_review_discloses_evidence_without_granting_read_root() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "no findings"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 21, "completion_tokens": 3, "total_tokens": 24}
        })))
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let repo = TempDir::new().unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["init", "--quiet"])
        .status()
        .unwrap()
        .success());
    fs::write(repo.path().join("review-me.rs"), "fn main() {}\n").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args(["--gateway", "example", "--no-stream", "review"]);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains(
            "bytes of host-verified evidence to example-model-v1",
        ))
        .stderr(predicate::str::contains("no read root granted"))
        .stderr(predicate::str::contains("read roots 1").not());

    let requests = server.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    for request in requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(
            body.get("tools")
                .map(serde_json::Value::is_null)
                .unwrap_or(true),
            "review advertised tools: {body}"
        );
        let prompt = body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(prompt.contains("review-me.rs"));
    }
}

// --- cycle 7 cheap wins -----------------------------------------------------

/// A clean tree is not a dead end: echo the path the user typed and name the
/// targets that would actually have something to review.
#[test]
#[serial]
fn test_review_on_clean_tree_suggests_reachable_targets() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let repo = TempDir::new().unwrap();
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
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/lib.rs"), "fn clean() {}\n").unwrap();
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

    let mut top_level = base_cmd(home.path());
    top_level
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args(["--gateway", "example", "review", "src/lib.rs"]);
    top_level
        .assert()
        .failure()
        .stderr(predicate::str::contains("at src/lib.rs"))
        .stderr(predicate::str::contains("--commit HEAD src/lib.rs"))
        .stderr(predicate::str::contains("@src/lib.rs"));

    let script = repo.path().join("review.txt");
    fs::write(&script, "/review src/lib.rs\n/quit\n").unwrap();
    let mut slash = base_cmd(home.path());
    slash
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &script.to_string_lossy(),
        ]);
    slash
        .assert()
        .failure()
        .stderr(predicate::str::contains("/review commit HEAD src/lib.rs"))
        .stderr(predicate::str::contains("@src/lib.rs"));
}

/// The preflight error explains its own numbers: window, effective prompt
/// budget, and the reserve that separates the target from it — and it names
/// `--context-window`, the most direct lever.
#[test]
#[serial]
fn test_context_preflight_error_explains_its_budget_numbers() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");

    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "--gateway",
        "example",
        "--context-window",
        "2048",
        "ask",
        &"overflowing prompt ".repeat(4000),
    ]);
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("trim target is"))
        .stderr(predicate::str::contains("effective prompt budget"))
        .stderr(predicate::str::contains("window 2048 tokens"))
        .stderr(predicate::str::contains("reserve)"))
        .stderr(predicate::str::contains("--context-window <n>"));
}

// --- cross-run artifact retention (cycle 7 item 4) --------------------------

/// A stateless `--enable-commands` run leaves its generation on disk after
/// exit — deliberately, so the captured output stays readable — and
/// `kitsu artifacts` is what finds and reclaims it afterwards.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_artifacts_list_and_prune_reclaim_cross_run_generations() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let argv = vec!["/bin/echo".to_string(), "KITSU_RETAINED".to_string()];
    let tool_args = serde_json::json!({"argv": argv, "cwd": work.path()}).to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "command_call_1",
                        "type": "function",
                        "function": {"name": "command_exec", "arguments": tool_args}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let pregrant = serde_json::to_string(&argv).unwrap();
    let mut cmd = base_cmd(home.path());
    let run = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(work.path())
        .args([
            "--gateway",
            "example",
            "--no-stream",
            "chat",
            "--no-session",
            "--command-root",
            &work.path().to_string_lossy(),
            "--allow-command",
            &pregrant,
            "run it",
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    // The generation survived process exit, and cross-run listing finds it.
    let mut list = base_cmd(home.path());
    let listed = list
        .args(["artifacts", "list", "--json"])
        .output()
        .expect("artifact inventory");
    assert!(listed.status.success());
    let inventory: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let generations = inventory["generations"].as_array().unwrap();
    let retained = generations
        .iter()
        .find(|generation| generation["namespace"] == "runtime")
        .expect("a retained runtime generation");
    assert_eq!(retained["state"], "orphaned");
    assert_eq!(retained["prunable"], true);
    assert!(retained["files"].as_u64().unwrap() >= 1);
    assert!(retained["bytes"].as_u64().unwrap() > 0);
    let generation_path = retained["path"].as_str().unwrap().to_string();
    assert!(Path::new(&generation_path).exists());

    // --dry-run reports without removing.
    let mut dry = base_cmd(home.path());
    dry.args(["artifacts", "prune", "all", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would remove 1 generation(s)"));
    assert!(Path::new(&generation_path).exists());

    let mut prune = base_cmd(home.path());
    prune
        .args(["artifacts", "prune", "all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1 generation(s)"));
    assert!(!Path::new(&generation_path).exists());

    // An unmatched selector is an error, consistent with `sessions delete`.
    let mut missing = base_cmd(home.path());
    missing
        .args(["artifacts", "prune", "runtime-0-0-0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no retained artifact generation"));
}

/// Pruning must never touch sessions, config, locks, or anything that is not an
/// artifact generation — and must refuse a symlink wearing a generation name.
#[test]
#[serial]
fn test_artifacts_prune_never_touches_sessions_or_foreign_entries() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let state = home.path().join(".kitsu");
    let artifacts = state.join("artifacts");
    let sessions = state.join("sessions");
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("precious.txt"), "do not delete\n").unwrap();
    fs::create_dir_all(&artifacts).unwrap();
    fs::create_dir_all(sessions.join(".locks")).unwrap();
    fs::create_dir_all(&state).unwrap();

    let generation = artifacts.join("runtime-11-22-0");
    fs::create_dir(&generation).unwrap();
    fs::write(generation.join(".lock"), "").unwrap();
    fs::write(generation.join("command-output-0.txt"), "removable\n").unwrap();
    fs::create_dir(artifacts.join("not-an-artifact")).unwrap();
    fs::write(artifacts.join("not-an-artifact/keep.txt"), "keep\n").unwrap();
    fs::write(sessions.join("s-1-2-0.json"), "{}").unwrap();
    fs::create_dir(sessions.join("s-1-2-0.artifacts")).unwrap();
    fs::write(sessions.join("s-1-2-0.artifacts/kept.txt"), "owned\n").unwrap();
    fs::write(state.join("history"), "line\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), artifacts.join("runtime-33-44-0")).unwrap();

    let mut prune = base_cmd(home.path());
    prune
        .args(["artifacts", "prune", "all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1 generation(s)"));

    assert!(!generation.exists());
    assert!(artifacts.join("not-an-artifact/keep.txt").exists());
    assert!(sessions.join("s-1-2-0.json").exists());
    assert!(sessions.join("s-1-2-0.artifacts/kept.txt").exists());
    assert!(sessions.join(".locks").exists());
    assert!(state.join("history").exists());
    assert!(state.join("config.toml").exists());
    assert!(outside.path().join("precious.txt").exists());
    #[cfg(unix)]
    assert!(fs::symlink_metadata(artifacts.join("runtime-33-44-0")).is_ok());
}

/// Listing creates nothing: inspecting disk state must not materialize a
/// profile directory.
#[test]
#[serial]
fn test_artifacts_list_is_read_only() {
    let home = TempDir::new().unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.args(["artifacts"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no retained command artifacts"));

    assert!(
        !home.path().join(".kitsu").exists(),
        "listing artifacts must not create Kitsu state"
    );
}

/// The startup sweep removes provably empty orphans and nothing else, and
/// `/artifacts` stops reporting "0 retained" while other runs sit on disk.
#[test]
#[serial]
fn test_startup_sweep_removes_only_empty_orphans_and_slash_reports_cross_run() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let state = home.path().join(".kitsu");
    let artifacts = state.join("artifacts");
    let sessions = state.join("sessions");
    fs::create_dir_all(&artifacts).unwrap();
    fs::create_dir_all(&sessions).unwrap();

    // Empty orphan generation with a released lock: sweepable.
    let empty = artifacts.join("runtime-11-22-0");
    fs::create_dir(&empty).unwrap();
    fs::write(empty.join(".lock"), "").unwrap();
    // Non-empty orphan: retained, never swept.
    let populated = artifacts.join("runtime-11-22-1");
    fs::create_dir(&populated).unwrap();
    fs::write(populated.join(".lock"), "").unwrap();
    fs::write(populated.join("command-output-0.txt"), "keep me\n").unwrap();
    // Empty orphan sidecar with a released lock: sweepable.
    let empty_sidecar = sessions.join("s-9-9-9.artifacts");
    fs::create_dir(&empty_sidecar).unwrap();
    fs::create_dir_all(sessions.join(".locks")).unwrap();
    fs::write(sessions.join(".locks/s-9-9-9.lock"), "").unwrap();
    // Sidecar whose session still exists: never swept.
    fs::write(sessions.join("s-1-2-0.json"), "{}").unwrap();
    let owned_sidecar = sessions.join("s-1-2-0.artifacts");
    fs::create_dir(&owned_sidecar).unwrap();

    let script = home.path().join("script.txt");
    fs::write(&script, "/artifacts\n/quit\n").unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
        "--gateway",
        "example",
        "chat",
        "--enable-commands",
        "--script",
        &script.to_string_lossy(),
    ]);
    cmd.assert()
        .success()
        .stderr(predicate::str::contains("other runs:"))
        .stderr(predicate::str::contains("kitsu artifacts list"));

    assert!(!empty.exists(), "empty orphan generation should be swept");
    assert!(
        !empty_sidecar.exists(),
        "empty orphan sidecar should be swept"
    );
    assert!(populated.exists(), "non-empty orphan must survive");
    assert!(owned_sidecar.exists(), "owned sidecar must survive");
    assert!(sessions.join("s-1-2-0.json").exists());
}

/// A generation whose lock is held by a live process is never removed, and the
/// prune says why rather than failing silently.
#[cfg(unix)]
#[test]
#[serial]
fn test_artifacts_prune_skips_live_generation() {
    use std::os::unix::io::AsRawFd as _;

    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let artifacts = home.path().join(".kitsu/artifacts");
    fs::create_dir_all(&artifacts).unwrap();
    let live = artifacts.join("runtime-55-66-0");
    fs::create_dir(&live).unwrap();
    fs::write(live.join("command-output-0.txt"), "in flight\n").unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(live.join(".lock"))
        .unwrap();
    // SAFETY: `lock` is an open descriptor owned by this test.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );

    let mut prune = base_cmd(home.path());
    prune
        .args(["artifacts", "prune", "all"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("removed 0 generation(s)"))
        .stdout(predicate::str::contains(
            "a live process holds this generation's lock",
        ))
        .stderr(predicate::str::contains("nothing was removed"));

    assert!(live.join("command-output-0.txt").exists());
    drop(lock);

    // Once the owner is gone the same command reclaims it.
    let mut again = base_cmd(home.path());
    again
        .args(["artifacts", "prune", "all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1 generation(s)"));
    assert!(!live.exists());
}

// --- cycle 10: model-callable workspace_diff evidence tool -------------------

/// Prepare a git repo with one committed file that has an uncommitted edit.
fn init_dirty_repo(repo: &Path) {
    let git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "--quiet"]);
    fs::write(repo.join("code.txt"), "before\n").unwrap();
    git(&["add", "code.txt"]);
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
    fs::write(repo.join("code.txt"), "after\n").unwrap();
}

/// The cycle-9 gap, closed: in a chat with no filesystem grants the model can
/// call `workspace_diff` and receive the host-verified diff, the disclosure is
/// announced like /review's, and no read root exists before or after.
#[tokio::test]
#[serial]
async fn test_workspace_diff_tool_discloses_evidence_in_scripted_chat() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "diff_call_1",
                        "type": "function",
                        "function": {
                            "name": "workspace_diff",
                            "arguments": "{\"scope\":\"working\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "your uncommitted change replaces before with after"
                },
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let repo = TempDir::new().unwrap();
    init_dirty_repo(repo.path());
    let script = repo.path().join("explain.txt");
    fs::write(
        &script,
        "explain what my uncommitted changes do\n/permissions\n/quit\n",
    )
    .unwrap();

    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let script_path = script.clone();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        let output = cmd
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .current_dir(&repo_path)
            .args([
                "--gateway",
                "example",
                "--no-stream",
                "chat",
                "--script",
                &script_path.to_string_lossy(),
            ])
            .output()
            .expect("run scripted workspace_diff chat");
        assert!(
            output.status.success(),
            "stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // The call surfaces like every other tool receipt, and the standing
        // disclosure rule applies at the moment of disclosure.
        assert!(stderr.contains("workspace_diff"), "{stderr}");
        assert!(
            stderr.contains("bytes of host-verified evidence to example-model-v1"),
            "{stderr}"
        );
        assert!(stderr.contains("no read root granted"), "{stderr}");
        // The diff-backed answer reached the transcript.
        assert!(
            stdout.contains("your uncommitted change replaces before with after"),
            "{stdout}"
        );
        // Still zero model read roots; /permissions names the tool as a
        // host-evidence consumer.
        assert!(stderr.contains("read roots   none"), "{stderr}");
        assert!(stderr.contains("workspace_diff tool"), "{stderr}");
        assert!(
            stderr.contains("workspace_diff discloses one bounded diff per model call"),
            "{stderr}"
        );
    })
    .await
    .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    // The tool is advertised and the system prompt teaches the model to fetch
    // verified diffs itself instead of flailing.
    let tools = first["tools"].as_array().expect("advertised tools");
    assert!(
        tools
            .iter()
            .any(|tool| tool["function"]["name"] == "workspace_diff"),
        "{first}"
    );
    let system = first["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("Use workspace_diff to fetch the user's verified git diff"),
        "{system}"
    );
    // The tool result fed back to the model carries the actual diff bytes.
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let tool_message = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("tool result message");
    let content = tool_message["content"].as_str().unwrap();
    assert!(content.contains("+after"), "{content}");
    assert!(content.contains("\"target\":\"working_tree\""), "{content}");
}

/// JSONL framing: the workspace_diff call is a normal tool frame, and the
/// disclosure is a `workspace_evidence_disclosure` event between
/// `tool_started` and `tool_finished`, shaped like `review_disclosure`.
#[tokio::test]
#[serial]
async fn test_workspace_diff_jsonl_emits_disclosure_event_between_tool_frames() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "diff_call_1",
                        "type": "function",
                        "function": {"name": "workspace_diff", "arguments": "{}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "explained"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let repo = TempDir::new().unwrap();
    init_dirty_repo(repo.path());
    let script = repo.path().join("explain.txt");
    fs::write(&script, "explain my changes\n/quit\n").unwrap();

    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let script_path = script.clone();
    let events = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        let output = cmd
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .current_dir(&repo_path)
            .args([
                "--gateway",
                "example",
                "--jsonl",
                "--no-stream",
                "chat",
                "--script",
                &script_path.to_string_lossy(),
            ])
            .output()
            .expect("run jsonl workspace_diff chat");
        assert!(
            output.status.success(),
            "stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap();

    let started = events
        .iter()
        .position(|event| event["type"] == "tool_started" && event["name"] == "workspace_diff")
        .expect("tool_started for workspace_diff");
    let finished = events
        .iter()
        .position(|event| event["type"] == "tool_finished" && event["name"] == "workspace_diff")
        .expect("tool_finished for workspace_diff");
    let disclosure = events
        .iter()
        .position(|event| event["type"] == "workspace_evidence_disclosure")
        .expect("workspace_evidence_disclosure event");
    assert!(started < disclosure && disclosure < finished);

    let disclosure = &events[disclosure];
    assert_eq!(disclosure["schema"], "kitsu.chat.v1");
    assert_eq!(disclosure["source"], "workspace_diff");
    assert_eq!(disclosure["target"], "working_tree");
    assert_eq!(disclosure["model_disclosed"], true);
    assert_eq!(disclosure["model_read_root_granted"], false);
    assert_eq!(disclosure["truncated"], false);
    assert!(disclosure["evidence_bytes"].as_u64().unwrap() > 0);

    // The tool result inside the frame carries the diff; no permission or
    // grant event appears anywhere in the stream.
    let finished = &events[finished];
    assert!(finished["output"].as_str().unwrap().contains("+after"));
    assert!(events
        .iter()
        .all(|event| event["type"] != "permission_prompt"));
}

/// Both opt-outs unregister the tool entirely, and a non-worktree directory
/// never registers it in the first place.
#[test]
#[serial]
fn test_no_workspace_evidence_flag_and_config_unregister_the_tool() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "http://127.0.0.1:1");
    let repo = TempDir::new().unwrap();
    init_dirty_repo(repo.path());
    let script = repo.path().join("tools.txt");
    fs::write(&script, "/tools\n/quit\n").unwrap();

    // Baseline: the worktree registers the tool.
    let mut with_tool = base_cmd(home.path());
    with_tool
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &script.to_string_lossy(),
        ]);
    with_tool
        .assert()
        .success()
        .stderr(predicate::str::contains("workspace_diff"));

    // Flag opt-out.
    let mut flagged = base_cmd(home.path());
    flagged
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--gateway",
            "example",
            "--no-workspace-evidence",
            "chat",
            "--script",
            &script.to_string_lossy(),
        ]);
    flagged
        .assert()
        .success()
        .stderr(predicate::str::contains("workspace_diff").not());

    // Config opt-out.
    let config_path = home.path().join(".kitsu/config.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        format!("workspace_evidence = false\n{config}"),
    )
    .unwrap();
    let mut configured = base_cmd(home.path());
    configured
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(repo.path())
        .args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &script.to_string_lossy(),
        ]);
    configured
        .assert()
        .success()
        .stderr(predicate::str::contains("workspace_diff").not());

    // Outside a git worktree there is nothing to disclose: not registered.
    fs::write(&config_path, config).unwrap();
    let plain = TempDir::new().unwrap();
    let plain_script = plain.path().join("tools.txt");
    fs::write(&plain_script, "/tools\n/quit\n").unwrap();
    let mut no_worktree = base_cmd(home.path());
    no_worktree
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(plain.path())
        .args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &plain_script.to_string_lossy(),
        ]);
    no_worktree
        .assert()
        .success()
        .stderr(predicate::str::contains("workspace_diff").not());
}
