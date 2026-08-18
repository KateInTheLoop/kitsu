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

#[path = "support/jsonl_fixture.rs"]
mod jsonl_fixture;

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
    // Selection-variable behavior is opt-in per test; ambient developer shell
    // defaults must not silently redirect the mock gateway or model.
    cmd.env_remove("KITSU_GATEWAY");
    cmd.env_remove("KITSU_MODEL");
    // Ensure no ambient expiry interferes.
    cmd.env_remove("ANTHROPIC_TOKEN_EXPIRES");
    // Don't auto-load the repo's own llms.txt into test system prompts.
    cmd.env("KITSU_NO_PROJECT_CONTEXT", "1");
    cmd
}

/// Run a plain interactive chat on a PTY and let a deterministic driver react
/// to the accumulated transcript. Keeping the PTY mechanics here makes the
/// confirmation-EOF and active-turn queue fixtures about lifecycle behavior,
/// not duplicated descriptor plumbing.
#[cfg(unix)]
fn run_plain_interactive_chat_pty<F>(
    home: &Path,
    cwd: &Path,
    extra_chat_args: &[&str],
    mut drive: F,
) -> (std::process::ExitStatus, String)
where
    F: FnMut(&[u8], &mut fs::File),
{
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let mut master = -1;
    let mut slave = -1;
    // SAFETY: both output pointers are valid; null termios and winsize request
    // the platform defaults.
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
        "allocate interactive carrier PTY"
    );
    // SAFETY: the master descriptor remains owned by this function. Making it
    // nonblocking lets the driver synchronize on prompts without hanging.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    // SAFETY: each duplicated slave descriptor transfers to one Stdio owner.
    let child_stdin = unsafe { libc::dup(slave) };
    let child_stdout = unsafe { libc::dup(slave) };
    assert!(child_stdin >= 0 && child_stdout >= 0);

    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
    command
        .env("HOME", home)
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .current_dir(cwd)
        // SAFETY: all three live descriptors transfer exactly once.
        .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
        .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
        .stderr(unsafe { Stdio::from_raw_fd(slave) })
        .args([
            "--plain",
            "--gateway",
            "example",
            "--no-stream",
            "chat",
            "--no-session",
        ])
        .args(extra_chat_args);
    let mut child = command.spawn().expect("spawn interactive carrier chat");
    // SAFETY: ownership of the remaining master descriptor transfers once.
    let mut terminal = unsafe { fs::File::from_raw_fd(master) };
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut transcript = Vec::new();

    let status = loop {
        let mut buffer = [0_u8; 4096];
        match terminal.read(&mut buffer) {
            Ok(0) => {}
            Ok(read) => transcript.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            // Linux PTY masters report EIO after the last slave closes.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("read interactive carrier PTY: {error}"),
        }
        drive(&transcript, &mut terminal);
        if let Some(status) = child.try_wait().expect("poll interactive carrier") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "interactive carrier timed out: {}",
                String::from_utf8_lossy(&transcript)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    (status, String::from_utf8_lossy(&transcript).into_owned())
}

#[test]
#[serial]
fn test_output_last_message_rejects_management_commands_without_touching_destination() {
    let home = TempDir::new().unwrap();
    let destination = home.path().join("last-message.txt");
    fs::write(&destination, "preserve").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.args([
        "--output-last-message",
        &destination.to_string_lossy(),
        "sessions",
    ]);
    cmd.assert()
        .failure()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains(
            "--output-last-message is supported only by non-interactive",
        ));
    assert_eq!(fs::read_to_string(destination).unwrap(), "preserve");
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
    let destination = home.path().join("last-message.txt");
    fs::write(&destination, "previous committed answer").unwrap();

    let mut cmd = base_cmd(home.path());
    cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.args([
        "--output-last-message",
        &destination.to_string_lossy(),
        "ask",
        "--gateway",
        "example",
        "hi",
    ]);

    cmd.assert()
        .failure()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("refresh-token"))
        .stderr(predicate::str::contains("kitsu-context").not())
        .stderr(predicate::str::contains("provider attempt").not())
        .stderr(predicate::str::contains("kitsu is thinking").not());
    assert_eq!(
        fs::read_to_string(destination).unwrap(),
        "previous committed answer"
    );
}

/// The offline doctor and inference preflight must share one bounded recovery
/// path. Doctor may disclose the configured env-var name and safe prose, but
/// never a value; model startup must fail before unrelated setup telemetry.
#[test]
#[serial]
fn test_missing_auth_preflight_and_doctor_share_safe_recovery_guidance() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "https://gateway.invalid");

    let mut doctor = base_cmd(home.path());
    doctor.env_remove("ANTHROPIC_AUTH_TOKEN");
    let output = doctor
        .args(["doctor", "--gateway", "example", "--json"])
        .output()
        .expect("run doctor");
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
    assert_eq!(report["auth"]["credential_present"], false);
    assert_eq!(
        report["auth"]["recovery_hint"],
        "run `refresh-token` to refresh the gateway JWT"
    );
    assert!(report["blocking_issues"]
        .as_array()
        .expect("blocking issues")
        .iter()
        .any(|issue| issue
            .as_str()
            .is_some_and(|issue| issue.contains("refresh-token"))));

    let mut inference = base_cmd(home.path());
    inference.env_remove("ANTHROPIC_AUTH_TOKEN");
    let output = inference
        .args(["doctor", "--gateway", "example", "--inference", "--json"])
        .output()
        .expect("run inference doctor without auth");
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["online"]["scope"], "chat_inference");
    assert_eq!(report["online"]["chat_route_probed"], false);
    assert_eq!(report["online"]["request_sent"], false);
    assert_eq!(report["online"]["potentially_billable"], true);
    assert!(report["online"]["status"]
        .as_str()
        .is_some_and(|status| status.contains("requested but skipped")));

    let mut models = base_cmd(home.path());
    models.env_remove("ANTHROPIC_AUTH_TOKEN");
    models
        .args(["models", "--gateway", "example"])
        .assert()
        .failure()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("refresh-token"));
}

#[test]
#[serial]
fn test_loopback_ollama_connection_failure_has_scoped_secret_free_recovery() {
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).expect("create ~/.kitsu");
    let url_secret = "kitsu-url-secret-633";
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "ollama"
[gateways.ollama]
flavor = "openai"
base_url = "http://user:{url_secret}@127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
model = "local-model"
context_window = 8192
auth = {{ method = "none" }}
"#
        ),
    )
    .expect("write config.toml");

    let output = base_cmd(home.path())
        .args([
            "--jsonl",
            "--no-stream",
            "--timeout-secs",
            "1",
            "ask",
            "hello",
        ])
        .output()
        .expect("run unreachable local Ollama ask");
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let error = events
        .iter()
        .find(|event| event["type"] == "error")
        .expect("typed provider error");
    assert_eq!(error["kind"], "provider");
    assert_eq!(error["exit_code"], 2);
    let message = error["message"].as_str().expect("error message");
    assert!(message.contains("local Ollama"), "{message}");
    assert!(
        message.contains("configured loopback endpoint"),
        "{message}"
    );
    assert!(message.contains("`ollama serve`"), "{message}");
    assert!(message.contains("select another gateway"), "{message}");
    assert!(!message.contains(url_secret), "{message}");
    assert!(!message.contains("127.0.0.1"), "{message}");
    assert_eq!(error["diagnostic_code"], "local_ollama_unreachable");
    assert_eq!(
        error["recovery_actions"],
        serde_json::json!([
            "start_ollama_serve",
            "verify_ollama_serve",
            "select_another_gateway",
        ])
    );
    assert_eq!(
        events.last().and_then(|event| event["type"].as_str()),
        Some("final_summary")
    );
    assert_eq!(events.last().unwrap()["provider_failures"], 1);

    let script = home.path().join("unreachable-local-ollama.script");
    fs::write(&script, "hello from a script\n").unwrap();
    let scripted = base_cmd(home.path())
        .args([
            "--jsonl",
            "--no-stream",
            "--timeout-secs",
            "1",
            "chat",
            "--no-session",
            "--no-project-context",
            "--no-tools",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("run unreachable local Ollama script");
    assert_eq!(scripted.status.code(), Some(2), "{scripted:?}");
    assert!(scripted.stderr.is_empty(), "{scripted:?}");
    let scripted_events = String::from_utf8(scripted.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let scripted_error = scripted_events
        .iter()
        .find(|event| event["type"] == "error")
        .expect("typed scripted provider error");
    assert_eq!(
        scripted_error["diagnostic_code"],
        "local_ollama_unreachable"
    );
    assert_eq!(
        scripted_error["recovery_actions"],
        error["recovery_actions"]
    );
    assert!(scripted_error["message"]
        .as_str()
        .is_some_and(|message| message.contains("`ollama serve`")));
    assert!(!scripted_error.to_string().contains(url_secret));
    assert_eq!(
        scripted_events
            .last()
            .and_then(|event| event["type"].as_str()),
        Some("final_summary")
    );
    assert_eq!(scripted_events.last().unwrap()["provider_failures"], 1);

    let doctor = base_cmd(home.path())
        .args(["--timeout-secs", "1", "doctor", "--online", "--json"])
        .output()
        .expect("run unreachable local Ollama doctor");
    assert_eq!(doctor.status.code(), Some(1), "{doctor:?}");
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["online"]["request_sent"], true);
    let status = report["online"]["status"].as_str().expect("online status");
    assert!(status.contains("local Ollama"), "{status}");
    assert!(status.contains("`ollama serve`"), "{status}");
    assert!(!status.contains(url_secret), "{status}");

    let models = base_cmd(home.path())
        .args(["--jsonl", "--timeout-secs", "1", "models"])
        .output()
        .expect("run unreachable local Ollama catalog");
    assert_eq!(models.status.code(), Some(2), "{models:?}");
    assert!(models.stderr.is_empty(), "{models:?}");
    let model_events = String::from_utf8(models.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        model_events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "run_started",
            "provider_request",
            "provider_attempt",
            "error",
            "final_summary",
        ]
    );
    assert_eq!(model_events[1]["operation_id"], "models:1");
    assert_eq!(model_events[1]["purpose"], "model_catalog");
    assert_eq!(model_events[1]["message_count"], 0);
    assert_eq!(model_events[1]["tools_offered"], 0);
    assert_eq!(model_events[1]["structured_output"], false);
    assert_eq!(model_events[1]["request_timeout_ms"], 1_000);
    assert_eq!(
        model_events[1]["turn_remaining_ms"],
        serde_json::Value::Null
    );
    assert_eq!(model_events[2]["operation_id"], "models:1");
    assert_eq!(model_events[2]["purpose"], "model_catalog");
    assert_eq!(model_events[2]["attempt"], 1);
    assert_eq!(model_events[2]["max_attempts"], 1);
    assert_eq!(model_events[2]["category"], "connection");
    assert_eq!(model_events[2]["status"], serde_json::Value::Null);
    assert_eq!(model_events[2]["outcome"], "transport_failed");
    assert_eq!(model_events[2]["terminal"], true);
    assert_eq!(model_events[2]["deadline_scope"], "provider_request");
    assert_eq!(
        model_events[2]["turn_remaining_ms"],
        serde_json::Value::Null
    );
    assert_eq!(
        model_events[3]["diagnostic_code"],
        "local_ollama_unreachable"
    );
    assert_eq!(
        model_events[3]["recovery_actions"],
        error["recovery_actions"]
    );
    assert!(!serde_json::to_string(&model_events)
        .unwrap()
        .contains(url_secret));
}

#[test]
#[serial]
fn test_doctor_reports_config_artifact_storage_disable() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let config = home.path().join(".kitsu/config.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("artifact_storage = false\n{contents}")).unwrap();

    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["artifact_policy"]["mode"], "disabled");
    assert_eq!(report["artifact_policy"]["source"], "config");
    assert_eq!(
        report["artifact_policy"]["new_automatic_artifact_writes"],
        false
    );
    assert_eq!(report["artifact_policy"]["tool_result_offload"], false);
    assert_eq!(
        report["artifact_policy"]["existing_artifacts_unchanged"],
        true
    );
}

#[test]
#[serial]
fn test_managed_auth_login_status_logout_is_provider_free_and_secret_safe() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "https://gateway.invalid");
    let secret = "test-token-managed-731";

    let mut login = base_cmd(home.path());
    login.env_remove("ANTHROPIC_AUTH_TOKEN");
    let output = login
        .args([
            "--gateway",
            "example",
            "auth",
            "login",
            "--token-stdin",
            "--json",
        ])
        .write_stdin(format!("{secret}\n"))
        .output()
        .expect("managed login");
    assert!(output.status.success(), "{:?}", output);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(secret));
    assert!(!combined.contains("credentials"));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stored"], true);
    assert_eq!(report["effective_source"], "managed");
    assert_eq!(report["remote_validity_checked"], false);
    assert_eq!(report["request_sent"], false);

    let credential_dir = home.path().join(".kitsu/credentials");
    let entries = fs::read_dir(&credential_dir)
        .expect("credential directory")
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        assert_eq!(
            fs::metadata(&credential_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let metadata = entries[0].metadata().unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
    }

    let rotated_secret = "test-token-managed-rotated-844";
    let mut rotate = base_cmd(home.path());
    let output = rotate
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args([
            "--gateway",
            "example",
            "auth",
            "login",
            "--token-stdin",
            "--json",
        ])
        .write_stdin(format!("{rotated_secret}\n"))
        .output()
        .expect("atomic managed rotation");
    assert!(output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(rotated_secret));
    assert_eq!(
        fs::read_to_string(entries[0].path()).unwrap(),
        rotated_secret
    );
    assert_eq!(fs::read_dir(&credential_dir).unwrap().count(), 1);

    let mut status = base_cmd(home.path());
    status.env_remove("ANTHROPIC_AUTH_TOKEN");
    let output = status
        .args(["--gateway", "example", "auth", "status", "--json"])
        .output()
        .expect("managed status");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["credential_present"], true);
    assert_eq!(report["managed_credential_present"], true);
    assert_eq!(report["environment_override_present"], false);
    assert_eq!(report["effective_source"], "managed");
    assert_eq!(report["locally_usable"], true);
    assert_eq!(report["request_sent"], false);

    let mut override_status = base_cmd(home.path());
    let output = override_status
        .env("ANTHROPIC_AUTH_TOKEN", "environment-override")
        .args(["--gateway", "example", "auth", "status", "--json"])
        .output()
        .expect("environment override status");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["effective_source"], "environment");
    assert_eq!(report["environment_override_present"], true);
    assert_eq!(report["managed_credential_present"], true);

    let mut malformed_override = base_cmd(home.path());
    let output = malformed_override
        .env("ANTHROPIC_AUTH_TOKEN", "malformed\nvalue")
        .args(["--gateway", "example", "auth", "status", "--json"])
        .output()
        .expect("malformed environment override status");
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["effective_source"], "environment");
    assert_eq!(report["managed_credential_present"], true);
    assert_eq!(report["locally_usable"], false);
    assert_eq!(report["request_sent"], false);

    let mut doctor = base_cmd(home.path());
    let output = doctor
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args(["--gateway", "example", "doctor", "--json"])
        .output()
        .expect("offline doctor with managed credential");
    assert!(output.status.success(), "{:?}", output);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["auth"]["effective_source"], "managed");
    assert_eq!(report["auth"]["managed_credential_present"], true);
    assert_eq!(report["online"]["request_sent"], false);

    let mut logout = base_cmd(home.path());
    let output = logout
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args(["--gateway", "example", "auth", "logout", "--json"])
        .output()
        .expect("managed logout");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["removed"], true);
    assert_eq!(report["effective_source"], "missing");
    assert!(!credential_dir.join(entries[0].file_name()).exists());

    let mut second_logout = base_cmd(home.path());
    let output = second_logout
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args(["--gateway", "example", "auth", "logout", "--json"])
        .output()
        .expect("idempotent logout");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["removed"], false);

    let mut missing = base_cmd(home.path());
    let output = missing
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args(["--gateway", "example", "auth", "status", "--json"])
        .output()
        .expect("missing managed status");
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["credential_present"], false);
    assert_eq!(report["request_sent"], false);
}

#[tokio::test]
#[serial]
async fn test_managed_auth_is_used_for_provider_requests_without_environment_export() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
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
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
model = "managed-model"
context_window = 8000
auth = {{ method = "bearer-env", env = "KITSU_TEST_MANAGED_REQUEST_TOKEN" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let secret = "managed-request-token-205";
    let mut login = base_cmd(home.path());
    login
        .env_remove("KITSU_TEST_MANAGED_REQUEST_TOKEN")
        .args([
            "--gateway",
            "example",
            "auth",
            "login",
            "--token-stdin",
            "--json",
        ])
        .write_stdin(format!("{secret}\n"))
        .assert()
        .success();

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", format!("Bearer {secret}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "managed-model"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut models = base_cmd(home.path());
    let output = models
        .env_remove("KITSU_TEST_MANAGED_REQUEST_TOKEN")
        .args(["--gateway", "example", "models"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "managed-model"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(secret));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
#[serial]
fn test_auth_login_requires_explicit_stdin_or_hidden_terminal() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "https://gateway.invalid");
    let mut login = base_cmd(home.path());
    login
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args(["--gateway", "example", "auth", "login", "--json"])
        .assert()
        .failure()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("--token-stdin"));
    assert!(!home.path().join(".kitsu/credentials").exists());
}

#[test]
fn test_auth_login_never_uses_temporary_state_when_home_is_missing() {
    let root = TempDir::new().expect("temp root");
    write_config(root.path(), "https://gateway.invalid");
    let config = root.path().join(".kitsu/config.toml");
    let secret = "test-token-no-home";
    let mut login = Command::cargo_bin("kitsu").unwrap();
    let output = login
        .env_remove("HOME")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .args([
            "--config",
            &config.to_string_lossy(),
            "--gateway",
            "example",
            "auth",
            "login",
            "--token-stdin",
            "--json",
        ])
        .write_stdin(format!("{secret}\n"))
        .output()
        .expect("login without HOME");
    assert_eq!(output.status.code(), Some(1));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("requires an absolute HOME"));
    assert!(!combined.contains(secret));
    assert!(!root.path().join(".kitsu/credentials").exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn test_auth_login_hides_tty_input_and_restores_echo_on_sigint() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    fn run_tty(home: &Path, input: Option<&str>) -> (std::process::ExitStatus, String, bool) {
        let mut master = -1;
        let mut slave = -1;
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
            0
        );
        let child_stdin = unsafe { libc::dup(slave) };
        let child_stdout = unsafe { libc::dup(slave) };
        let probe_slave = unsafe { libc::dup(slave) };
        assert!(child_stdin >= 0 && child_stdout >= 0 && probe_slave >= 0);
        let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );

        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
        command
            .env("HOME", home)
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env("KITSU_NO_PROJECT_CONTEXT", "1")
            .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
            .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
            .stderr(unsafe { Stdio::from_raw_fd(slave) })
            .args(["--gateway", "example", "auth", "login", "--json"]);
        let mut child = command.spawn().expect("spawn auth login on PTY");
        let mut terminal = unsafe { fs::File::from_raw_fd(master) };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut transcript = Vec::new();
        let mut acted = false;
        let status = loop {
            let mut buffer = [0_u8; 2048];
            match terminal.read(&mut buffer) {
                Ok(0) => {}
                Ok(read) => transcript.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
                Err(error) => panic!("read auth PTY: {error}"),
            }
            if !acted
                && transcript
                    .windows(b"credential: ".len())
                    .any(|window| window == b"credential: ")
            {
                if let Some(input) = input {
                    terminal.write_all(input.as_bytes()).unwrap();
                    terminal.write_all(b"\n").unwrap();
                    terminal.flush().unwrap();
                } else {
                    assert_eq!(
                        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
                        0
                    );
                }
                acted = true;
            }
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "auth PTY timed out: {}",
                    String::from_utf8_lossy(&transcript)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(probe_slave, termios.as_mut_ptr()) },
            0
        );
        let echo_restored = unsafe { termios.assume_init() }.c_lflag & libc::ECHO != 0;
        unsafe { libc::close(probe_slave) };
        (
            status,
            String::from_utf8_lossy(&transcript).into_owned(),
            echo_restored,
        )
    }

    let success_home = TempDir::new().unwrap();
    write_config(success_home.path(), "https://gateway.invalid");
    let secret = "test-token-hidden-421";
    let (status, transcript, echo_restored) = run_tty(success_home.path(), Some(secret));
    assert!(status.success(), "{status}: {transcript}");
    assert!(transcript.contains("credential: "));
    assert!(!transcript.contains(secret));
    assert!(echo_restored);

    let cancel_home = TempDir::new().unwrap();
    write_config(cancel_home.path(), "https://gateway.invalid");
    let (status, transcript, echo_restored) = run_tty(cancel_home.path(), None);
    assert_eq!(status.code(), Some(130), "{status}: {transcript}");
    assert!(
        echo_restored,
        "terminal echo was not restored: {transcript}"
    );
    assert!(!cancel_home.path().join(".kitsu/credentials").exists());
}

#[cfg(unix)]
#[test]
#[serial]
fn test_managed_auth_rejects_hard_links_and_intersecting_command_roots() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "https://gateway.invalid");
    let secret = "test-token-private-913";
    let mut login = base_cmd(home.path());
    login
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args([
            "--gateway",
            "example",
            "auth",
            "login",
            "--token-stdin",
            "--json",
        ])
        .write_stdin(format!("{secret}\n"))
        .assert()
        .success();

    let empty_script = home.path().join("empty.script");
    fs::write(&empty_script, "# provider-free\n").unwrap();
    let mut broad_command = base_cmd(home.path());
    broad_command
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args([
            "--gateway",
            "example",
            "chat",
            "--script",
            &empty_script.to_string_lossy(),
            "--command-root",
            &home.path().to_string_lossy(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "intersects Kitsu's managed-credential namespace",
        ));

    let credential = fs::read_dir(home.path().join(".kitsu/credentials"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let alias = home.path().join("credential-hard-link-sentinel");
    fs::hard_link(&credential, &alias).unwrap();
    let mut status = base_cmd(home.path());
    let output = status
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args(["--gateway", "example", "auth", "status", "--json"])
        .output()
        .expect("status with unsafe hard link");
    assert_eq!(output.status.code(), Some(1));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(secret));
    assert!(!combined.contains(&credential.to_string_lossy().to_string()));
    assert!(combined.contains("managed credential store is unavailable or unsafe"));

    fs::remove_file(&alias).unwrap();
    fs::remove_file(&credential).unwrap();
    let symlink_target = home.path().join("credential-symlink-target");
    fs::write(&symlink_target, "outside-sentinel").unwrap();
    std::os::unix::fs::symlink(&symlink_target, &credential).unwrap();
    let mut symlink_status = base_cmd(home.path());
    let output = symlink_status
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .args(["--gateway", "example", "auth", "status", "--json"])
        .output()
        .expect("status with symlinked credential leaf");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(&symlink_target).unwrap(),
        "outside-sentinel"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(&symlink_target.to_string_lossy().to_string()));
    assert!(combined.contains("managed credential store is unavailable or unsafe"));
}

#[test]
#[serial]
fn test_managed_auth_is_isolated_by_config_and_gateway() {
    let home = TempDir::new().unwrap();
    let config_body = |default: &str| {
        format!(
            r#"default_gateway = "{default}"
[gateways.alpha]
flavor = "openai"
base_url = "https://gateway.invalid"
chat_path = "/v1/chat/completions"
models_path = ""
model = "test"
auth = {{ method = "bearer-env", env = "KITSU_TEST_ISOLATED_TOKEN" }}
[gateways.beta]
flavor = "openai"
base_url = "https://gateway.invalid"
chat_path = "/v1/chat/completions"
models_path = ""
model = "test"
auth = {{ method = "bearer-env", env = "KITSU_TEST_ISOLATED_TOKEN" }}
"#
        )
    };
    let config_one = home.path().join("one.toml");
    let config_two = home.path().join("two.toml");
    fs::write(&config_one, config_body("alpha")).unwrap();
    fs::write(&config_two, config_body("alpha")).unwrap();

    let mut login = base_cmd(home.path());
    login
        .env_remove("KITSU_TEST_ISOLATED_TOKEN")
        .args([
            "--config",
            &config_one.to_string_lossy(),
            "--gateway",
            "alpha",
            "auth",
            "login",
            "--token-stdin",
            "--json",
        ])
        .write_stdin("isolated-token\n")
        .assert()
        .success();

    for (config, gateway, expected_code) in [
        (&config_one, "alpha", 0),
        (&config_one, "beta", 1),
        (&config_two, "alpha", 1),
    ] {
        let mut status = base_cmd(home.path());
        let output = status
            .env_remove("KITSU_TEST_ISOLATED_TOKEN")
            .args([
                "--config",
                &config.to_string_lossy(),
                "--gateway",
                gateway,
                "auth",
                "status",
                "--json",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected_code), "{:?}", output);
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["managed_credential_present"], expected_code == 0);
        assert_eq!(report["request_sent"], false);
    }
}

#[test]
fn test_auth_status_is_truthful_for_none_and_static_profiles() {
    let home = TempDir::new().unwrap();
    let config = home.path().join("profiles.toml");
    let static_secret = "static-test-secret-633";
    fs::write(
        &config,
        format!(
            r#"default_gateway = "local"
[gateways.local]
flavor = "openai"
base_url = "http://127.0.0.1:1"
chat_path = "/v1/chat/completions"
models_path = ""
model = "local"
auth = {{ method = "none" }}
[gateways.inline]
flavor = "openai"
base_url = "https://gateway.invalid"
chat_path = "/v1/chat/completions"
models_path = ""
model = "hosted"
auth = {{ method = "static", token = "{static_secret}" }}
"#
        ),
    )
    .unwrap();

    for (gateway, source, present) in [("local", "none", false), ("inline", "static", true)] {
        let mut status = base_cmd(home.path());
        let output = status
            .args([
                "--config",
                &config.to_string_lossy(),
                "--gateway",
                gateway,
                "auth",
                "status",
                "--json",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!combined.contains(static_secret));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["effective_source"], source);
        assert_eq!(report["credential_present"], present);
        assert_eq!(report["managed_credential_present"], false);
        assert_eq!(report["locally_usable"], true);
        assert_eq!(report["request_sent"], false);
    }

    let mut login = base_cmd(home.path());
    login
        .args([
            "--config",
            &config.to_string_lossy(),
            "--gateway",
            "local",
            "auth",
            "login",
            "--token-stdin",
        ])
        .write_stdin("unused-secret\n")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "auth login supports bearer-env gateways",
        ));
    assert!(!home.path().join(".kitsu/credentials").exists());
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

#[test]
fn test_prompt_flag_rejects_an_explicit_subcommand() {
    let home = TempDir::new().expect("temp home");
    let mut cmd = base_cmd(home.path());
    cmd.args(["-p", "one-shot", "ask", "second prompt"]);
    cmd.assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "-p/--prompt is a top-level one-shot mode",
        ));
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
        .stdout("")
        .stderr(predicate::str::contains("refresh-token"))
        .stderr(predicate::str::contains("script summary").not())
        .stderr(predicate::str::contains("kitsu-context").not());
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
    let output = chat
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "chat",
            "--session",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("run empty durable script");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let started = events
        .iter()
        .find(|event| event["type"] == "run_started")
        .expect("run_started");
    assert!(started["session_id"].is_string());
    assert_eq!(
        started["session_snapshot_state"],
        "pending_until_first_success"
    );
    let summary = events.last().expect("final_summary");
    assert_eq!(summary["type"], "final_summary");
    assert!(summary["session_id"].is_null());
    assert_eq!(
        summary["session_snapshot_state"],
        "pending_until_first_success"
    );

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
        // This session-lifecycle test submits complete canonical lines before
        // reading output. Select literal input explicitly so it does not rely
        // on an ambient NO_COLOR setting or race Rustyline's raw-mode startup.
        .args(["--plain", "chat"]);
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

#[cfg(unix)]
#[test]
#[serial]
fn test_terminal_mode_pty_matrix_decouples_rendering_and_input() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    fn run_case(name: &str, args: &[&str], env: &[(&str, &str)]) -> Vec<u8> {
        let home = TempDir::new().expect("temp home");
        write_config(home.path(), "http://127.0.0.1:1");

        let mut master = -1;
        let mut slave = -1;
        // SAFETY: both output pointers are valid; null termios and winsize
        // request the platform defaults.
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
            "allocate PTY for {name}"
        );
        // SAFETY: `master` is a live descriptor and F_GETFL/F_SETFL do not
        // change its ownership. Nonblocking reads let the test synchronize on
        // the visible prompt without racing Rustyline's raw-mode setup.
        let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
        assert!(flags >= 0, "read PTY flags for {name}");
        assert_eq!(
            unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0,
            "set nonblocking PTY reads for {name}"
        );

        // SAFETY: each duplicated live descriptor is transferred exactly once
        // to a Stdio owner below.
        let child_stdin = unsafe { libc::dup(slave) };
        let child_stdout = unsafe { libc::dup(slave) };
        assert!(child_stdin >= 0 && child_stdout >= 0);

        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
        cmd.env("HOME", home.path())
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .env("KITSU_NO_PROJECT_CONTEXT", "1")
            .env("TERM", "xterm-256color")
            .env_remove("ANTHROPIC_TOKEN_EXPIRES")
            .env_remove("NO_COLOR")
            .env_remove("KITSU_PLAIN")
            .env_remove("KITSU_SCREEN_READER")
            .env_remove("KITSU_REDUCED_MOTION")
            // SAFETY: the descriptors are valid and transferred to their
            // Stdio owners exactly once.
            .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
            .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
            .stderr(unsafe { Stdio::from_raw_fd(slave) });
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.args(args).args(["chat", "--no-session"]);

        let mut child = cmd
            .spawn()
            .unwrap_or_else(|error| panic!("spawn {name}: {error}"));
        // SAFETY: `master` is the remaining live descriptor and ownership is
        // transferred to this File exactly once.
        let mut terminal = unsafe { fs::File::from_raw_fd(master) };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut submitted_exit = false;
        let status = loop {
            let mut buffer = [0_u8; 4096];
            match terminal.read(&mut buffer) {
                Ok(0) => {}
                Ok(read) => output.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                // Linux PTY masters report EIO after the last slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
                Err(error) => panic!("read {name} PTY: {error}"),
            }
            if !submitted_exit && output.windows(b"kitsu>".len()).any(|w| w == b"kitsu>") {
                terminal
                    // Ctrl+D is EOF at an empty canonical prompt and Rustyline's
                    // EOF binding in raw mode, so one byte exits both input paths.
                    .write_all(b"\x04")
                    .unwrap_or_else(|error| panic!("submit EOF for {name}: {error}"));
                submitted_exit = true;
            }
            if let Some(status) = child
                .try_wait()
                .unwrap_or_else(|error| panic!("poll {name}: {error}"))
            {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "{name} terminal mode did not exit; output: {}",
                    String::from_utf8_lossy(&output)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(submitted_exit, "{name} never displayed an input prompt");
        assert!(status.success(), "{name} failed: {status}");
        output
    }

    let cases = [
        ("default", vec![], vec![], false, true),
        ("NO_COLOR", vec![], vec![("NO_COLOR", "1")], false, false),
        ("--plain", vec!["--plain"], vec![], true, false),
        ("TERM=dumb", vec![], vec![("TERM", "dumb")], true, false),
        (
            "--reduced-motion",
            vec!["--reduced-motion"],
            vec![],
            false,
            true,
        ),
        (
            "--screen-reader",
            vec!["--screen-reader"],
            vec![],
            true,
            false,
        ),
    ];
    for (name, args, env, assistive, styled) in cases {
        let output = run_case(name, &args, &env);
        let bracketed_paste = output
            .windows(b"\x1b[?2004h".len())
            .any(|window| window == b"\x1b[?2004h");
        assert_eq!(bracketed_paste, !assistive, "{name}: {output:?}");
        if assistive {
            assert!(
                !output.contains(&b'\x1b'),
                "{name} emitted terminal escapes: {output:?}"
            );
        }
        let fox_truecolor = output
            .windows(b"\x1b[38;2;255;140;0m".len())
            .any(|window| window == b"\x1b[38;2;255;140;0m");
        assert_eq!(fox_truecolor, styled, "{name}: {output:?}");
        assert!(
            !output
                .windows(b"\x1b]".len())
                .any(|window| window == b"\x1b]"),
            "{name} emitted an OSC sequence: {output:?}"
        );
    }
}

#[cfg(unix)]
#[test]
#[serial]
fn test_slash_palette_and_footer_fit_a_fifty_column_pty() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    use unicode_width::UnicodeWidthStr as _;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");

    let mut master = -1;
    let mut slave = -1;
    let size = libc::winsize {
        ws_row: 24,
        ws_col: 50,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: output pointers and the initialized winsize remain valid for the
    // call; openpty initializes both descriptors on success.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        },
        0,
        "allocate narrow PTY"
    );
    // SAFETY: `master` is live and these fcntl calls do not transfer ownership.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    // SAFETY: each duplicate is independently transferred to one Stdio owner.
    let child_stdin = unsafe { libc::dup(slave) };
    let child_stdout = unsafe { libc::dup(slave) };
    assert!(child_stdin >= 0 && child_stdout >= 0);

    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
    cmd.env("HOME", home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env("TERM", "xterm-256color")
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .env_remove("NO_COLOR")
        .env_remove("KITSU_PLAIN")
        .env_remove("KITSU_SCREEN_READER")
        // SAFETY: the live descriptors transfer to the child exactly once.
        .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
        .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
        .stderr(unsafe { Stdio::from_raw_fd(slave) })
        .args(["chat", "--no-session"]);
    let mut child = cmd.spawn().expect("spawn Kitsu in narrow PTY");
    // SAFETY: ownership of the remaining master descriptor transfers here.
    let mut terminal = unsafe { fs::File::from_raw_fd(master) };
    let mut output = Vec::new();
    let mut opened_palette = false;
    let mut cleared_palette = false;
    let mut sent_eof = false;
    let deadline = Instant::now() + Duration::from_secs(5);

    let status = loop {
        let mut buffer = [0_u8; 4096];
        match terminal.read(&mut buffer) {
            Ok(0) => {}
            Ok(read) => output.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("read narrow PTY: {error}"),
        }
        if !opened_palette && output.windows(6).any(|window| window == b"kitsu>") {
            terminal.write_all(b"/").expect("open slash palette");
            opened_palette = true;
        }
        if opened_palette
            && !cleared_palette
            && output
                .windows("slash commands".len())
                .any(|window| window == b"slash commands")
        {
            terminal.write_all(b"\x03").expect("clear slash palette");
            cleared_palette = true;
        }
        if cleared_palette
            && !sent_eof
            && output
                .windows("input cleared".len())
                .any(|window| window == b"input cleared")
        {
            terminal.write_all(b"\x04").expect("exit narrow PTY");
            sent_eof = true;
        }
        if let Some(status) = child.try_wait().expect("poll narrow PTY") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "narrow PTY did not exit; output: {}",
                String::from_utf8_lossy(&output)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "narrow Kitsu failed: {status}");
    assert!(opened_palette && cleared_palette && sent_eof);

    let visible = kitsu_render::sanitize_terminal_text(&String::from_utf8_lossy(&output));
    let lines = visible.lines().collect::<Vec<_>>();
    let palette_start = lines
        .iter()
        .position(|line| line.starts_with("╭ slash commands"))
        .unwrap_or_else(|| panic!("slash palette missing from narrow PTY: {visible}"));
    let mut saw_status = false;
    for line in &lines[palette_start..] {
        if line.starts_with('╭')
            || line.starts_with('│')
            || line.starts_with('├')
            || line.starts_with('╰')
        {
            assert_eq!(line.width(), 50, "wrapped narrow surface row: {line:?}");
        }
        if line.starts_with("╰ model ") {
            saw_status = true;
            break;
        }
    }
    assert!(saw_status, "responsive status footer missing: {visible}");
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
#[serial]
fn test_scripted_sessions_filter_handles_missing_namespace_provider_free() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("filtered-empty-sessions.script");
    fs::write(&script, "/sessions Needle-That-Is-Not-Saved\n/quit\n").unwrap();

    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--no-tools",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("filter sessions in a fresh namespace");
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
    assert_eq!(results.len(), 1, "{events:?}");
    let result = &results[0]["result"];
    assert_eq!(result["kind"], "sessions");
    assert_eq!(result["count"], 0);
    assert_eq!(result["filtered_count"], 0);
    assert!(result["current_session_id"].is_null());
    assert_eq!(result["sessions"], serde_json::json!([]));
    assert!(result["text"]
        .as_str()
        .unwrap()
        .contains("no sessions matched 'Needle-That-Is-Not-Saved'"));
    let summary = events.last().expect("final summary");
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["status"], "ok");
    assert_eq!(summary["failures"], 0);
    assert_eq!(summary["successful_operations"], 1);
    assert!(
        !home.path().join(".kitsu/sessions").exists(),
        "a read-only empty search created the session namespace"
    );
}

#[test]
#[serial]
fn test_jsonl_help_registry_exposes_canonical_alias_relationships_provider_free() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("typed-help.script");
    fs::write(&script, "/help\n/quit\n").unwrap();

    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--no-tools",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("render typed help registry");
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
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "run_started",
            "command_started",
            "command_result",
            "command_finished",
            "command_started",
            "command_finished",
            "final_summary",
        ]
    );
    let result = &events[2]["result"];
    assert_eq!(result["kind"], "help");
    let commands = result["commands"].as_array().expect("typed commands");
    assert!(commands
        .iter()
        .all(|command| command.get("alias_of").is_some()));
    let command = |name: &str| {
        commands
            .iter()
            .find(|command| command["name"] == name)
            .unwrap_or_else(|| panic!("missing /{name}: {commands:?}"))
    };
    assert_eq!(command("agents")["alias_of"], "jobs");
    assert_eq!(command("quit")["alias_of"], "exit");
    assert!(command("jobs")["alias_of"].is_null());
    assert!(command("exit")["alias_of"].is_null());
    assert!(result["text"]
        .as_str()
        .unwrap()
        .contains("(alias: /agents)"));
    assert!(result["text"].as_str().unwrap().contains("(alias: /quit)"));
    let summary = events.last().expect("final summary");
    assert_eq!(summary["status"], "ok");
    assert_eq!(summary["successful_operations"], 1);
    assert_eq!(summary["failures"], 0);
    assert!(!events
        .iter()
        .any(|event| event["type"] == "provider_request"));
    assert!(
        !home.path().join(".kitsu/sessions").exists(),
        "provider-free help created session state"
    );
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

#[test]
fn test_chat_no_tools_conflicts_with_capability_grants() {
    let cases = [
        (vec!["--allow-write", "."], "--allow-write"),
        (vec!["--allow-read-write", "."], "--allow-read-write"),
        (vec!["--enable-commands"], "--enable-commands"),
        (vec!["--command-root", "."], "--command-root"),
        (
            vec!["--command-runtime-root", "."],
            "--command-runtime-root",
        ),
        (vec!["--command-env", "PATH"], "--command-env"),
        (
            vec!["--allow-command", "[\"cargo\",\"test\"]"],
            "--allow-command",
        ),
    ];

    for (capability, expected_flag) in cases {
        let mut cmd = Command::cargo_bin("kitsu").expect("kitsu binary should build");
        cmd.args(["chat", "--no-tools"]).args(capability);
        cmd.assert()
            .failure()
            .code(1)
            .stderr(predicate::str::contains("cannot be used with"))
            .stderr(predicate::str::contains("--no-tools"))
            .stderr(predicate::str::contains(expected_flag));
    }
}

#[tokio::test]
#[serial]
async fn test_chat_no_tools_omits_every_model_tool() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "tool-free answer"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let attachment = home.path().join("tool-free-context.txt");
    fs::write(&attachment, "TOOL_FREE_ATTACHMENT_MARKER").unwrap();
    let home_path = home.path().to_path_buf();
    let prompt = format!("summarize @{}", attachment.to_string_lossy());
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
            "--no-stream",
            "chat",
            "--no-tools",
            "--allow-path",
            &home_path.to_string_lossy(),
            &prompt,
        ]);
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("tool-free answer"));
    })
    .await
    .expect("spawned tool-free chat should join");

    let requests = server.received_requests().await.expect("record request");
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(body.to_string().contains("TOOL_FREE_ATTACHMENT_MARKER"));
    assert!(
        body.get("tools")
            .map(serde_json::Value::is_null)
            .unwrap_or(true),
        "--no-tools request advertised tools: {body}"
    );
}

#[tokio::test]
#[serial]
async fn test_chat_no_tools_composes_with_named_session_fork_and_resume_after_rebase() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "tool-free answer"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        })))
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut first = base_cmd(&home_path);
        let output = first
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--no-stream",
                "chat",
                "--no-tools",
                "--session",
                "--name",
                "merged-no-tools",
                "first saved turn",
            ])
            .output()
            .expect("create named tool-free session");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let mut list = base_cmd(&home_path);
        let output = list
            .args(["sessions", "--json"])
            .output()
            .expect("list named session");
        assert!(output.status.success(), "{:?}", output);
        let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["name"], "merged-no-tools");
        let id = listed[0]["id"].as_str().unwrap().to_string();

        let mut fork = base_cmd(&home_path);
        let output = fork
            .args(["sessions", "--json", "fork", "last"])
            .output()
            .expect("fork named tool-free session");
        assert!(output.status.success(), "{:?}", output);
        let forked: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(forked["source_session_id"], id);
        assert_eq!(forked["permissions_restored"], false);
        let forked_id = forked["session"]["id"].as_str().unwrap().to_string();
        assert!(
            forked["session"]["name"].is_null(),
            "a fork must not duplicate its source's resume selector: {forked}"
        );

        let mut second = base_cmd(&home_path);
        let output = second
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--no-stream",
                "chat",
                "--no-tools",
                "--resume",
                "merged-no-tools",
                "second saved turn",
            ])
            .output()
            .expect("resume named tool-free session");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let mut show = base_cmd(&home_path);
        let output = show
            .args(["sessions", "--json", "show", &id])
            .output()
            .expect("show resumed tool-free session");
        assert!(output.status.success(), "{:?}", output);
        let saved: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(saved["name"], "merged-no-tools");
        let transcript = saved["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(transcript.contains("first saved turn"), "{transcript}");
        assert!(transcript.contains("second saved turn"), "{transcript}");

        let mut show_fork = base_cmd(&home_path);
        let output = show_fork
            .args(["sessions", "--json", "show", &forked_id])
            .output()
            .expect("show forked tool-free session");
        assert!(output.status.success(), "{:?}", output);
        let forked: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(forked["name"].is_null());
        let forked_transcript = forked["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            forked_transcript.contains("first saved turn"),
            "{forked_transcript}"
        );
        assert!(
            !forked_transcript.contains("second saved turn"),
            "fork transcript changed with its source: {forked_transcript}"
        );

        let mut human_fork = base_cmd(&home_path);
        let output = human_fork
            .args(["sessions", "fork", &id])
            .output()
            .expect("render human fork lineage");
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.starts_with(&format!("forked session {id} -> s-")),
            "{stdout}"
        );
        assert!(stdout.contains("permissions were not copied"), "{stdout}");
    })
    .await
    .expect("tool-free named-session process should join");

    let requests = server.received_requests().await.expect("record requests");
    assert_eq!(requests.len(), 2);
    for request in requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(
            body.get("tools")
                .map(serde_json::Value::is_null)
                .unwrap_or(true),
            "named --no-tools request advertised merged tool schemas: {body}"
        );
    }
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_scripted_session_catalog_handles_filters_and_archived_recovery() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "searchable saved answer"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let first = base_cmd(&home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--no-stream",
                "chat",
                "--no-tools",
                "--session",
                "--name",
                "Searchable-Demo",
                "first saved turn",
            ])
            .output()
            .expect("create searchable named session");
        assert!(
            first.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&first.stderr)
        );

        let listed = base_cmd(&home_path)
            .args(["sessions", "--json"])
            .output()
            .expect("list searchable session");
        assert!(listed.status.success(), "{listed:?}");
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        let session_id = listed[0]["id"].as_str().unwrap().to_string();
        assert_eq!(listed[0]["name"], "Searchable-Demo");
        assert_eq!(listed[0]["turns"], 1);

        let jsonl_script = home_path.join("filtered-sessions-jsonl.script");
        fs::write(
            &jsonl_script,
            "/sessions sEaRcHaBlE-dEmO\n/sessions no-such-session\n/quit\n",
        )
        .unwrap();
        let jsonl = base_cmd(&home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "chat",
                "--no-tools",
                "--resume",
                "Searchable-Demo",
                "--script",
                &jsonl_script.to_string_lossy(),
            ])
            .output()
            .expect("filter a named session in JSONL mode");
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
        let results = events
            .iter()
            .filter(|event| event["type"] == "command_result")
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 2, "{events:?}");

        let hit = &results[0]["result"];
        assert_eq!(hit["kind"], "sessions");
        assert_eq!(hit["count"], 1);
        assert_eq!(hit["filtered_count"], 1);
        assert_eq!(hit["current_session_id"], session_id);
        assert_eq!(hit["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(hit["sessions"][0]["id"], session_id);
        assert_eq!(hit["sessions"][0]["name"], "Searchable-Demo");
        assert!(hit["text"]
            .as_str()
            .unwrap()
            .contains(" * Searchable-Demo ("));

        let miss = &results[1]["result"];
        assert_eq!(miss["kind"], "sessions");
        assert_eq!(miss["count"], 1);
        assert_eq!(miss["filtered_count"], 0);
        assert_eq!(miss["current_session_id"], session_id);
        assert_eq!(miss["sessions"], serde_json::json!([]));
        assert!(miss["text"]
            .as_str()
            .unwrap()
            .contains("no sessions matched 'no-such-session'"));

        let summary = events.last().expect("final summary");
        assert_eq!(summary["type"], "final_summary");
        assert_eq!(summary["status"], "ok");
        assert_eq!(summary["failures"], 0);
        assert_eq!(summary["successful_operations"], 2);
        assert_eq!(summary["session_id"], session_id);

        let human_script = home_path.join("filtered-sessions-human.script");
        fs::write(
            &human_script,
            "/help\n/sessions SEARCHABLE-demo\n/sessions absent-human\n/quit\n",
        )
        .unwrap();
        let human = base_cmd(&home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--no-stream",
                "chat",
                "--no-tools",
                "--resume",
                "Searchable-Demo",
                "--script",
                &human_script.to_string_lossy(),
            ])
            .output()
            .expect("filter a named session in human mode");
        assert!(human.status.success(), "{human:?}");
        let human_stderr = String::from_utf8_lossy(&human.stderr);
        assert!(
            human_stderr.contains("/sessions [filter]"),
            "{human_stderr}"
        );
        assert!(
            human_stderr.contains(" * Searchable-Demo ("),
            "{human_stderr}"
        );
        assert!(
            human_stderr.contains("no sessions matched 'absent-human'"),
            "{human_stderr}"
        );

        let shown = base_cmd(&home_path)
            .args(["sessions", "--json", "show", &session_id])
            .output()
            .expect("show session after provider-free filters");
        assert!(shown.status.success(), "{shown:?}");
        let snapshot_before_archive = shown.stdout.clone();
        let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(
            shown["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["role"] == "user")
                .count(),
            1
        );
        let transcript = shown["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(transcript.contains("first saved turn"), "{transcript}");
        assert!(
            transcript.contains("searchable saved answer"),
            "{transcript}"
        );
        assert!(!transcript.contains("/sessions"), "{transcript}");
        assert!(!transcript.contains("no-such-session"), "{transcript}");

        let archived = base_cmd(&home_path)
            .args(["sessions", "archive", &session_id])
            .output()
            .expect("archive the only live session");
        assert!(archived.status.success(), "{archived:?}");

        let archived_resume = base_cmd(&home_path)
            .args(["sessions", "--json", "resume", "Searchable-Demo"])
            .output()
            .expect("try to resume archived session by name");
        assert_eq!(
            archived_resume.status.code(),
            Some(1),
            "{archived_resume:?}"
        );

        let archived_script = home_path.join("archived-empty-sessions.script");
        fs::write(&archived_script, "/sessions\n/quit\n").unwrap();
        let archived_catalog = base_cmd(&home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "chat",
                "--no-session",
                "--no-tools",
                "--script",
                &archived_script.to_string_lossy(),
            ])
            .output()
            .expect("inspect live sessions while the only snapshot is archived");
        assert!(archived_catalog.status.success(), "{archived_catalog:?}");
        assert!(archived_catalog.stderr.is_empty());
        let archived_events = String::from_utf8(archived_catalog.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let catalog = archived_events
            .iter()
            .find(|event| event["type"] == "command_result")
            .expect("empty live session catalog");
        assert_eq!(catalog["result"]["kind"], "sessions");
        assert_eq!(catalog["result"]["count"], 0);
        assert_eq!(catalog["result"]["filtered_count"], 0);
        let guidance = catalog["result"]["text"].as_str().unwrap();
        assert!(guidance.contains("/restore <exact-id>"), "{guidance}");
        assert!(!guidance.contains("/resume"), "{guidance}");
        let archived_summary = archived_events.last().expect("archived catalog summary");
        assert_eq!(archived_summary["status"], "ok");
        assert_eq!(archived_summary["successful_operations"], 1);

        let restored = base_cmd(&home_path)
            .args(["sessions", "--json", "restore", &session_id])
            .output()
            .expect("restore exact archived session");
        assert!(restored.status.success(), "{restored:?}");
        let restored: serde_json::Value = serde_json::from_slice(&restored.stdout).unwrap();
        assert_eq!(restored["action"], "restore");
        assert_eq!(restored["session"]["id"], session_id);
        assert_eq!(restored["session"]["name"], "Searchable-Demo");

        let resolved = base_cmd(&home_path)
            .args(["sessions", "--json", "resume", "Searchable-Demo"])
            .output()
            .expect("resolve restored session by name");
        assert!(resolved.status.success(), "{resolved:?}");
        let resolved: serde_json::Value = serde_json::from_slice(&resolved.stdout).unwrap();
        assert_eq!(resolved["session_id"], session_id);

        let restored_show = base_cmd(&home_path)
            .args(["sessions", "--json", "show", &session_id])
            .output()
            .expect("show exact restored snapshot");
        assert!(restored_show.status.success(), "{restored_show:?}");
        assert_eq!(
            restored_show.stdout, snapshot_before_archive,
            "archive/restore changed the saved snapshot"
        );
    })
    .await
    .expect("scripted session filters should join");

    let requests = server.received_requests().await.expect("record requests");
    assert_eq!(requests.len(), 1, "filters must remain provider-free");
    server.verify().await;
}

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
async fn test_scripted_clear_preserves_old_session_and_rotates_jsonl_identity() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const OLD_MARKER: &str = "CLEAR_BOUNDARY_OLD_731";
    const NEW_MARKER: &str = "CLEAR_BOUNDARY_NEW_947";

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "saved boundary answer"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11}
        })))
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("clear-boundary.txt");
    fs::write(
        &script,
        format!("remember {OLD_MARKER}\n/clear\nremember {NEW_MARKER}\n"),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();

    let (events, old_id, new_id, old_saved, new_saved, listed) =
        tokio::task::spawn_blocking(move || {
            let mut run = base_cmd(&home_path);
            let output = run
                .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
                .args([
                    "--jsonl",
                    "--no-stream",
                    "--timeout-secs",
                    "10",
                    "chat",
                    "--session",
                    "--no-tools",
                    "--script",
                    script.to_str().unwrap(),
                ])
                .output()
                .expect("run scripted clear boundary");
            assert!(
                output.status.success(),
                "stderr: {}\nstdout: {}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                output.stderr.is_empty(),
                "JSONL leaked human diagnostics: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
                .collect();
            let started = events
                .iter()
                .find(|event| event["type"] == "run_started")
                .expect("run_started");
            let old_id = started["session_id"]
                .as_str()
                .expect("initial durable id")
                .to_string();
            let clear = events
                .iter()
                .find(|event| event["type"] == "command_result" && event["command"] == "/clear")
                .expect("typed clear receipt");
            let receipt = &clear["result"];
            let new_id = receipt["session_id"]
                .as_str()
                .expect("fresh durable id")
                .to_string();
            assert_ne!(new_id, old_id);
            assert_eq!(receipt["kind"], "clear_session_boundary");
            assert_eq!(receipt["previous_session_id"], old_id);
            assert_eq!(receipt["removed_turns"], 1);
            assert_eq!(receipt["removed_messages"], 2);
            assert_eq!(receipt["history_preserved"], true);
            assert_eq!(receipt["previous_session_resumable"], true);
            assert_eq!(
                receipt["new_session_snapshot_state"],
                "pending_until_first_success"
            );
            assert_eq!(receipt["runtime_grants_reset"], true);
            assert!(receipt["artifact_warning"].is_null());
            let summary = events
                .iter()
                .find(|event| event["type"] == "final_summary")
                .expect("final_summary");
            assert_eq!(summary["status"], "ok");
            assert_eq!(summary["session_id"], new_id);
            assert_eq!(summary["session_snapshot_state"], "resumable");

            let mut list = base_cmd(&home_path);
            let output = list
                .args(["sessions", "--json"])
                .output()
                .expect("list split sessions");
            assert!(output.status.success(), "{:?}", output);
            let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

            let show = |id: &str| {
                let mut command = base_cmd(&home_path);
                let output = command
                    .args(["sessions", "--json", "show", id])
                    .output()
                    .expect("show split session");
                assert!(output.status.success(), "{:?}", output);
                serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
            };
            let old_saved = show(&old_id);
            let new_saved = show(&new_id);
            (events, old_id, new_id, old_saved, new_saved, listed)
        })
        .await
        .expect("scripted clear process should join");

    let transcript = |saved: &serde_json::Value| {
        saved["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let old_transcript = transcript(&old_saved);
    let new_transcript = transcript(&new_saved);
    assert!(old_transcript.contains(OLD_MARKER), "{old_transcript}");
    assert!(!old_transcript.contains(NEW_MARKER), "{old_transcript}");
    assert!(new_transcript.contains(NEW_MARKER), "{new_transcript}");
    assert!(!new_transcript.contains(OLD_MARKER), "{new_transcript}");
    let sessions = listed.as_array().expect("session inventory");
    assert_eq!(sessions.len(), 2, "{listed}");
    for id in [&old_id, &new_id] {
        let saved = sessions
            .iter()
            .find(|summary| summary["id"] == *id)
            .expect("both split sessions listed");
        assert_eq!(saved["turns"], 1);
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "command_result" && event["command"] == "/clear")
            .count(),
        1
    );

    let requests = server.received_requests().await.expect("record requests");
    assert_eq!(requests.len(), 2);
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let first = first.to_string();
    let second = second.to_string();
    assert!(first.contains(OLD_MARKER), "{first}");
    assert!(!first.contains(NEW_MARKER), "{first}");
    assert!(second.contains(NEW_MARKER), "{second}");
    assert!(!second.contains(OLD_MARKER), "{second}");
    server.verify().await;
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
        assert_eq!(forked["source_session_id"], id);
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
        .expect(3)
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

        let mut human = base_cmd(&home_path);
        human
            .args([
                "--config",
                &config.to_string_lossy(),
                "doctor",
                "--online",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "provider check: reachable — model catalog only; chat/inference access was not tested",
            ))
            .stdout(predicate::str::contains(
                "doctor: no blocking issues found — run `kitsu` to verify chat/inference access",
            ))
            .stdout(predicate::str::contains("you're ready").not());
    })
    .await
    .expect("online doctor process should join");

    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_doctor_online_accepts_only_local_ollama_implicit_latest_without_chat() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"id": "llama3.2:latest"},
                {"id": "qwen2.5-coder:latest"}
            ]
        })))
        .expect(5)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    let config = home.path().join("doctor-local-ollama.toml");
    fs::write(
        &config,
        format!(
            r#"default_gateway = "ollama"
[gateways.ollama]
flavor = "openai"
base_url = "{}"
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
model = "llama3.2"
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut json_alias = base_cmd(&home_path);
        let output = json_alias
            .args([
                "--config",
                &config.to_string_lossy(),
                "doctor",
                "--online",
                "--json",
            ])
            .output()
            .expect("run JSON doctor for implicit latest");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["online"]["scope"], "model_catalog_only");
        assert_eq!(report["online"]["chat_route_probed"], false);
        assert_eq!(report["online"]["effective_model"], "llama3.2");
        assert_eq!(report["online"]["effective_model_available"], true);
        assert!(report["blocking_issues"].as_array().unwrap().is_empty());

        let mut human_alias = base_cmd(&home_path);
        human_alias
            .args([
                "--config",
                &config.to_string_lossy(),
                "doctor",
                "--online",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "gateway/model: ollama/llama3.2",
            ))
            .stdout(predicate::str::contains(
                "provider check: reachable — model catalog only; chat/inference access was not tested",
            ))
            .stdout(predicate::str::contains(
                "doctor: no blocking issues found — run `kitsu` to verify chat/inference access",
            ));

        let mut json_missing = base_cmd(&home_path);
        let output = json_missing
            .args([
                "--config",
                &config.to_string_lossy(),
                "--model",
                "missing-model",
                "doctor",
                "--online",
                "--json",
            ])
            .output()
            .expect("run JSON doctor for missing model");
        assert_eq!(output.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["online"]["chat_route_probed"], false);
        assert_eq!(report["online"]["effective_model_available"], false);
        assert_eq!(report["blocking_issues"].as_array().unwrap().len(), 1);

        let mut human_missing = base_cmd(&home_path);
        human_missing
            .args([
                "--config",
                &config.to_string_lossy(),
                "--model",
                "missing-model",
                "doctor",
                "--online",
            ])
            .assert()
            .failure()
            .code(1)
            .stdout(predicate::str::contains("doctor: 1 blocking issue(s)"))
            .stdout(predicate::str::contains(
                "effective model 'missing-model' is not advertised by the normalized gateway catalog",
            ));

        let mut explicit_tag = base_cmd(&home_path);
        let output = explicit_tag
            .args([
                "--config",
                &config.to_string_lossy(),
                "--model",
                "llama3.2:7b",
                "doctor",
                "--online",
                "--json",
            ])
            .output()
            .expect("run JSON doctor for explicit tag mismatch");
        assert_eq!(output.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["online"]["effective_model"], "llama3.2:7b");
        assert_eq!(report["online"]["effective_model_available"], false);
    })
    .await
    .expect("local Ollama doctor processes should join");

    let requests = server
        .received_requests()
        .await
        .expect("record model-catalog requests");
    assert_eq!(requests.len(), 5);
    for request in requests {
        assert_eq!(request.method.as_str(), "GET");
        assert_eq!(request.url.path(), "/v1/models");
    }
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_doctor_inference_probe_is_explicit_minimal_and_machine_readable() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"content": "OK"},
                "finish_reason": "stop"
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    let config = home.path().join("doctor-inference.toml");
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
                "--inference",
                "--json",
            ])
            .output()
            .expect("run inference doctor");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["online"]["requested"], true);
        assert_eq!(report["online"]["scope"], "chat_inference");
        assert_eq!(report["online"]["chat_route_probed"], true);
        assert_eq!(report["online"]["potentially_billable"], true);
        assert_eq!(report["online"]["request_sent"], true);
        assert_eq!(report["online"]["status"], "reachable");
        assert_eq!(report["online"]["effective_model"], "doctor-model");

        let mut human = base_cmd(&home_path);
        human
            .args([
                "--config",
                &config.to_string_lossy(),
                "doctor",
                "--inference",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "provider check: reachable — chat/inference accepted one minimal tool-free completion",
            ))
            .stdout(predicate::str::contains(
                "doctor: inference probe succeeded — the configured chat route accepted this model",
            ));
    })
    .await
    .expect("inference doctor process should join");

    let requests = server
        .received_requests()
        .await
        .expect("record inference requests");
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(request.method.as_str(), "POST");
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], "doctor-model");
        assert_eq!(
            body["messages"],
            serde_json::json!([{"role": "user", "content": "Reply OK."}])
        );
        assert_eq!(body["stream"], false);
        assert_eq!(body["max_tokens"], 1);
        assert!(body.get("tools").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_doctor_inference_classifies_access_billing_model_and_quota_failures() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let cases = [
        (
            "unauthorized",
            401_u16,
            "authentication/access was rejected",
        ),
        ("forbidden", 403_u16, "was forbidden"),
        ("billing", 402_u16, "billing or quota"),
        ("model", 404_u16, "rejected the chat route or model"),
        ("quota", 429_u16, "rate-limited"),
    ];
    for (route, status, _) in cases {
        Mock::given(method("POST"))
            .and(path(format!("/probe/{route}")))
            .respond_with(
                ResponseTemplate::new(status).set_body_string(format!("provider-secret-{route}")),
            )
            .expect(1)
            .mount(&server)
            .await;
    }

    let home = TempDir::new().expect("temp home");
    let mut configs = Vec::new();
    for (route, _, expected) in cases {
        let config = home.path().join(format!("doctor-{route}.toml"));
        fs::write(
            &config,
            format!(
                r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
chat_path = "/probe/{route}"
models_path = ""
model = "doctor-model"
auth = {{ method = "none" }}
"#,
                server.uri()
            ),
        )
        .unwrap();
        configs.push((config, route, expected));
    }
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        for (config, route, expected) in configs {
            let mut doctor = base_cmd(&home_path);
            let output = doctor
                .args([
                    "--config",
                    &config.to_string_lossy(),
                    "doctor",
                    "--inference",
                    "--json",
                ])
                .output()
                .expect("run failing inference doctor");
            assert_eq!(output.status.code(), Some(1), "route {route}");
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["online"]["scope"], "chat_inference");
            assert_eq!(report["online"]["chat_route_probed"], true);
            assert_eq!(report["online"]["request_sent"], true);
            let status = report["online"]["status"].as_str().unwrap();
            assert!(status.contains(expected), "route {route}: {status}");
            assert!(!status.contains(&format!("provider-secret-{route}")));
        }
    })
    .await
    .expect("failing inference doctor processes should join");

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
    let last_message = home.path().join("last-message.txt");
    let last_message_for_run = last_message.clone();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args([
            "--jsonl",
            "--reasoning-effort",
            "high",
            "-o",
            &last_message_for_run.to_string_lossy(),
            "ask",
            "hello",
        ]);
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
        let facts =
            jsonl_fixture::inspect_jsonl(&events, output.status.code().expect("process exit code"))
                .expect("inspect successful ask JSONL");
        assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::Succeeded);
        assert_eq!(
            facts.assistant,
            jsonl_fixture::JsonlAssistantDisposition::Committed
        );
        assert_eq!(facts.terminal_count, 1);
        assert_eq!(facts.exit_code, 0);
        assert_eq!(facts.provider_requests, 1);
        assert_eq!(facts.provider_attempts, 1);
        assert_eq!(facts.tool_started, 0);
        assert_eq!(facts.tool_finished, 0);
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
        let turn_finished_index = events
            .iter()
            .position(|event| event["type"] == "turn_finished")
            .expect("terminal turn frame");
        let output_index = events
            .iter()
            .position(|event| event["type"] == "output_last_message")
            .expect("typed output receipt");
        assert!(disposition_index < turn_finished_index && turn_finished_index < output_index);
        assert_eq!(events[output_index]["turn_id"], "ask:1");
        assert_eq!(
            events[output_index]["destination"],
            last_message_for_run.to_string_lossy().as_ref()
        );
        assert_eq!(events[output_index]["bytes"], "json answer".len());
        assert_eq!(
            events[output_index]["sha256"],
            "47267c917dcf68d6884df621a7b9d2fc20f77735d93e1255d8a4872df7403ddd"
        );
        assert_eq!(events[output_index]["atomic"], true);
        assert_eq!(events[output_index]["assistant_committed"], true);
        assert_eq!(events[output_index]["content_included"], false);
        assert_eq!(events[output_index]["model_write_authority"], false);
        assert_eq!(fs::read(&last_message_for_run).unwrap(), b"json answer");
        assert_eq!(events.last().unwrap()["type"], "final_summary");
        assert_eq!(events.last().unwrap()["status"], "ok");
        assert_eq!(events.last().unwrap()["successful_operations"], 1);
        assert!(!stdout.lines().any(|line| !line.starts_with('{')));
        assert!(
            output.stderr.is_empty(),
            "routine diagnostics leaked to JSONL stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    })
    .await
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&last_message).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

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
async fn test_output_schema_is_forwarded_validated_and_framed_before_commit() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"},"count":{"type":"integer","minimum":0}},"required":["status","count"],"additionalProperties":false}"#;
    const ANSWER: &str = r#"{"status":"ok","count":3}"#;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": ANSWER},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let schema_path = home.path().join("answer.schema.json");
    let destination = home.path().join("last-message.json");
    fs::write(&schema_path, SCHEMA).unwrap();
    let home_path = home.path().to_path_buf();
    let schema_for_run = schema_path.clone();
    let destination_for_run = destination.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--output-schema",
                &schema_for_run.to_string_lossy(),
                "--output-last-message",
                &destination_for_run.to_string_lossy(),
                "ask",
                "return the requested object",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read_to_string(&destination).unwrap(), ANSWER);
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let index = |kind: &str| {
        events
            .iter()
            .position(|event| event["type"] == kind)
            .unwrap_or_else(|| panic!("missing {kind}: {events:#?}"))
    };
    let schema_index = index("output_schema");
    assert!(index("run_started") < schema_index && schema_index < index("turn_started"));
    let receipt = &events[schema_index];
    assert_eq!(receipt["name"], "kitsu_output");
    assert_eq!(receipt["bytes"], SCHEMA.len());
    assert_eq!(
        receipt["sha256"],
        "ab2d534e73e0ac7a64401d7901b76a6c7b739e12e0842cc6a85b523391cc18b1"
    );
    assert_eq!(receipt["strict"], true);
    assert_eq!(receipt["provider_format"], "json_schema");
    assert_eq!(receipt["schema_preflight_validated"], true);
    assert_eq!(receipt["local_validation"], true);
    assert_eq!(receipt["schema_content_included"], false);
    assert_eq!(receipt["model_file_authority"], false);
    assert!(receipt.get("path").is_none());
    let provider = &events[index("provider_request")];
    assert_eq!(provider["structured_output"], true);
    assert!(index("assistant") < index("assistant_disposition"));
    assert_eq!(
        events[index("assistant_disposition")]["status"],
        "committed"
    );
    assert!(index("turn_finished") < index("output_last_message"));
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert_eq!(events.last().unwrap()["status"], "ok");
    assert_eq!(events.last().unwrap()["successful_operations"], 1);
    assert_eq!(events.last().unwrap()["output_schema_failures"], 0);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(
        body["response_format"]["json_schema"]["name"],
        "kitsu_output"
    );
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        body["response_format"]["json_schema"]["schema"],
        serde_json::from_str::<serde_json::Value>(SCHEMA).unwrap()
    );
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_output_schema_mismatch_discards_turn_and_preserves_output_destination() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"},"count":{"type":"integer","minimum":0}},"required":["status","count"],"additionalProperties":false}"#;
    // Exact failure shape from Cycle 23: syntactically valid JSON that violates
    // one schema keyword must be rejected with machine-readable diagnostics.
    const INVALID: &str = r#"{"status":"wrong","count":3}"#;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": INVALID},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let schema_path = home.path().join("answer.schema.json");
    let destination = home.path().join("last-message.json");
    fs::write(&schema_path, SCHEMA).unwrap();
    fs::write(&destination, "previous committed value").unwrap();
    let home_path = home.path().to_path_buf();
    let schema_for_run = schema_path.clone();
    let destination_for_run = destination.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--output-schema",
                &schema_for_run.to_string_lossy(),
                "-o",
                &destination_for_run.to_string_lossy(),
                "ask",
                "return the requested object",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "previous committed value"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events
        .iter()
        .any(|event| event["type"] == "provider_request" && event["structured_output"] == true));
    assert!(events.iter().any(|event| {
        event["type"] == "assistant_disposition"
            && event["status"] == "discarded"
            && event["reason"]
                .as_str()
                .unwrap()
                .contains("failed --output-schema validation")
    }));
    let error = events
        .iter()
        .find(|event| event["type"] == "error")
        .unwrap();
    assert_eq!(error["kind"], "output_schema");
    assert_eq!(error["exit_code"], 2);
    assert_eq!(
        error["schema_sha256"],
        "ab2d534e73e0ac7a64401d7901b76a6c7b739e12e0842cc6a85b523391cc18b1"
    );
    assert_eq!(error["instance_path"], "$/status");
    assert_eq!(error["validation_keyword"], "const");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("does not equal const"));
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["failures"], 1);
    assert_eq!(summary["provider_failures"], 1);
    assert_eq!(summary["output_schema_failures"], 1);
    assert_eq!(summary["tool_failures"], 0);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_invalid_output_schema_fails_before_provider_and_preserves_destination() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let schema = home.path().join("unsupported.schema.json");
    let destination = home.path().join("last-message.json");
    fs::write(
        &schema,
        r#"{"type":"object","properties":{},"required":[],"additionalProperties":false,"format":"unsupported"}"#,
    )
    .unwrap();
    fs::write(&destination, "preserve").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-schema",
            &schema.to_string_lossy(),
            "-o",
            &destination.to_string_lossy(),
            "ask",
            "hello",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read_to_string(&destination).unwrap(), "preserve");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .unwrap()
                .contains("unsupported keyword 'format'")
    }));
    assert!(events.iter().all(|event| {
        event["type"] != "run_started"
            && event["type"] != "output_schema"
            && event["type"] != "provider_request"
            && event["type"] != "output_last_message"
    }));
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn test_output_schema_file_is_protected_from_model_filesystem_writes() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"},"count":{"type":"integer","minimum":0}},"required":["status","count"],"additionalProperties":false}"#;
    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let schema = work.path().join("answer.schema.json");
    fs::write(&schema, SCHEMA).unwrap();
    let arguments = serde_json::json!({
        "path": schema.to_string_lossy(),
        "content": "model attempted schema overwrite"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": r#"{"status":"ok","count":1}"#},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
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
                        "id": "overwrite_schema",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("schema-protection.script");
    fs::write(&script, "try the requested operation\n").unwrap();
    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();
    let schema_for_run = schema.clone();
    let script_for_run = script.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--output-schema",
                &schema_for_run.to_string_lossy(),
                "chat",
                "--no-session",
                "--allow-read-write",
                &work_path.to_string_lossy(),
                "--script",
                &script_for_run.to_string_lossy(),
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read_to_string(&schema).unwrap(), SCHEMA);
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let tool = events
        .iter()
        .find(|event| event["type"] == "tool_finished")
        .expect("protected write result");
    assert_eq!(tool["success"], false);
    let tool_error: serde_json::Value =
        serde_json::from_str(tool["output"].as_str().unwrap()).unwrap();
    assert_eq!(tool_error["error"], "PolicyDenied");
    assert!(tool_error["detail"]
        .as_str()
        .unwrap()
        .contains("Kitsu-private"));
    assert_eq!(events.last().unwrap()["policy_failures"], 1);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_output_schema_failure_rolls_back_context_but_reports_retained_file_effects() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"}},"required":["status"],"additionalProperties":false}"#;
    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let effect = work.path().join("effect.txt");
    let arguments = serde_json::json!({
        "path": effect.to_string_lossy(),
        "content": "completed tool effect\n",
        "expected_current_content": null
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "```json\n{\"status\":\"ok\"}\n```"
                },
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
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
                        "id": "create_effect",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let schema = home.path().join("answer.schema.json");
    let script = home.path().join("retained-effect.script");
    let destination = home.path().join("last-message.json");
    fs::write(&schema, SCHEMA).unwrap();
    fs::write(&script, "create the requested effect and return JSON\n").unwrap();
    fs::write(&destination, "previous committed value").unwrap();
    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();
    let schema_for_run = schema.clone();
    let script_for_run = script.clone();
    let destination_for_run = destination.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--output-schema",
                &schema_for_run.to_string_lossy(),
                "-o",
                &destination_for_run.to_string_lossy(),
                "chat",
                "--no-session",
                "--allow-read-write",
                &work_path.to_string_lossy(),
                "--script",
                &script_for_run.to_string_lossy(),
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(
        fs::read_to_string(&effect).unwrap(),
        "completed tool effect\n"
    );
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "previous committed value"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let schema_error = events
        .iter()
        .find(|event| event["type"] == "error" && event["kind"] == "output_schema")
        .expect("typed output schema error");
    assert_eq!(schema_error["exit_code"], 2);
    assert_eq!(
        schema_error["schema_sha256"],
        "95d025be509b57ee1b4c07f8a5541e50c1c635a7fbe1fe260dfeb044f5dcefaa"
    );
    assert_eq!(schema_error["instance_path"], "$");
    assert_eq!(schema_error["validation_keyword"], "json_syntax");
    let retained = events
        .iter()
        .find(|event| event["type"] == "rollback_retained_changes")
        .expect("retained effect must be machine-visible");
    assert_eq!(retained["change_count"], 1);
    assert!(retained["paths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str() == Some(effect.to_string_lossy().as_ref())));
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let summary = events.last().unwrap();
    assert_eq!(summary["provider_failures"], 1);
    assert_eq!(summary["output_schema_failures"], 1);
    assert_eq!(summary["recovered_tool_errors"], 0);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_required_write_piped_success_is_verified_before_output_install() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let required = work.path().join("required-result.txt");
    let arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "verified result\n"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "TASK COMPLETE"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
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
                        "id": "write_required_result",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let destination = home.path().join("last-message.txt");
    let expected_sha256 =
        "db095dd5d3123eae56051704b1a97ad1fb51bd17c9f4cef481e43cffdaca26dd".to_ascii_uppercase();
    let exact_requirement = format!("{}={}", required.to_string_lossy(), expected_sha256);
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "-o",
            &destination.to_string_lossy(),
            "chat",
            "--no-session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write-sha256",
            &exact_requirement,
        ])
        .write_stdin("write the required result, then finish\n")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(fs::read(&required).unwrap(), b"verified result\n");
    assert_eq!(fs::read(&destination).unwrap(), b"TASK COMPLETE");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let postconditions = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event["type"] == "completion_postcondition")
        .collect::<Vec<_>>();
    assert_eq!(postconditions.len(), 2, "events: {events:#?}");
    assert_eq!(postconditions[0].1["phase"], "armed");
    assert_eq!(postconditions[0].1["status"], "pending");
    assert_eq!(postconditions[0].1["write_authority_granted"], false);
    assert_eq!(
        postconditions[0].1["expectations"],
        serde_json::json!([{
            "path": required.to_string_lossy(),
            "sha256": "db095dd5d3123eae56051704b1a97ad1fb51bd17c9f4cef481e43cffdaca26dd"
        }])
    );
    let (evaluated_index, evaluated) = postconditions[1];
    assert_eq!(evaluated["phase"], "evaluated");
    assert_eq!(evaluated["status"], "satisfied");
    assert_eq!(evaluated["results"][0]["satisfied"], true);
    assert_eq!(evaluated["results"][0]["journal_recorded"], true);
    assert_eq!(
        evaluated["results"][0]["current_matches_latest_write"],
        true
    );
    assert_eq!(evaluated["results"][0]["final_state_changed"], true);
    assert!(evaluated["results"][0]["initial_sha256"].is_null());
    assert_eq!(evaluated["results"][0]["final_bytes"], 16);
    assert_eq!(
        evaluated["results"][0]["expected_sha256"],
        "db095dd5d3123eae56051704b1a97ad1fb51bd17c9f4cef481e43cffdaca26dd"
    );
    assert_eq!(evaluated["results"][0]["expected_sha256_matches"], true);
    let output_index = events
        .iter()
        .position(|event| event["type"] == "output_last_message")
        .unwrap();
    assert!(evaluated_index < output_index && output_index < events.len() - 1);
    let summary = events.last().unwrap();
    assert_eq!(summary["status"], "ok");
    assert_eq!(summary["postcondition_failures"], 0);
    assert_eq!(summary["user_failures"], 0);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let first_request = String::from_utf8_lossy(&requests[0].body);
    assert!(first_request.contains("Host-enforced completion postcondition"));
    assert!(first_request.contains(required.to_string_lossy().as_ref()));
    assert!(
        first_request.contains("db095dd5d3123eae56051704b1a97ad1fb51bd17c9f4cef481e43cffdaca26dd")
    );
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_required_write_sha256_mismatch_fails_and_preserves_output() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let required = work.path().join("exact-result.txt");
    let arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "wrong exact result\n"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "TASK COMPLETE"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
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
                        "id": "write_wrong_exact_result",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let destination = home.path().join("last-message.txt");
    fs::write(&destination, "preserved host output").unwrap();
    let exact_requirement = format!(
        "{}={}",
        required.to_string_lossy(),
        "67db563e8cbb230fdef94e09b9961f99375986d4f9d90bf1e393998ad6a3bcb4"
    );
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "-o",
            &destination.to_string_lossy(),
            "chat",
            "--no-session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write-sha256",
            &exact_requirement,
        ])
        .write_stdin("write the exact required result, then finish\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(fs::read(&required).unwrap(), b"wrong exact result\n");
    assert_eq!(fs::read(&destination).unwrap(), b"preserved host output");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let evaluated = events
        .iter()
        .find(|event| event["type"] == "completion_postcondition" && event["phase"] == "evaluated")
        .expect("evaluated exact write receipt");
    assert_eq!(evaluated["status"], "failed");
    assert_eq!(evaluated["results"][0]["journal_recorded"], true);
    assert_eq!(evaluated["results"][0]["journal_net_content_changed"], true);
    assert_eq!(
        evaluated["results"][0]["current_matches_latest_write"],
        true
    );
    assert_eq!(evaluated["results"][0]["final_state_changed"], true);
    assert_eq!(
        evaluated["results"][0]["final_sha256"],
        "224fa499e3f4053c86c2b86733108cf6ffe3640a765d0cc8258d89965fc6552d"
    );
    assert_eq!(
        evaluated["results"][0]["expected_sha256"],
        "67db563e8cbb230fdef94e09b9961f99375986d4f9d90bf1e393998ad6a3bcb4"
    );
    assert_eq!(evaluated["results"][0]["expected_sha256_matches"], false);
    assert!(evaluated["results"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("does not match required"));
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let error = events
        .iter()
        .find(|event| event["type"] == "error" && event["kind"] == "postcondition")
        .expect("typed exact postcondition failure");
    assert_eq!(error["exit_code"], 1);
    let summary = events.last().unwrap();
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["failures"], 1);
    assert_eq!(summary["postcondition_failures"], 1);
    assert_eq!(summary["user_failures"], 1);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_required_write_existing_target_needs_read_authority_before_provider() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let work = TempDir::new().unwrap();
    let required = work.path().join("existing-result.txt");
    fs::write(&required, "launch contents\n").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write",
            &required.to_string_lossy(),
        ])
        .write_stdin("replace the required result\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read_to_string(&required).unwrap(), "launch contents\n");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().all(|event| {
        event["type"] != "run_started"
            && event["type"] != "provider_request"
            && event["type"] != "completion_postcondition"
    }));
    let error = events
        .iter()
        .find(|event| event["type"] == "error")
        .expect("typed preflight error");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("already exists"), "{message}");
    assert!(message.contains("--allow-read-write"), "{message}");
    assert!(message.contains("--allow-write alone is only valid for creating a new file"));
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn test_required_write_sha256_equal_to_launch_state_fails_before_provider() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let work = TempDir::new().unwrap();
    let required = work.path().join("already-exact.txt");
    fs::write(&required, "launch contents\n").unwrap();
    let exact_requirement = format!(
        "{}={}",
        required.to_string_lossy(),
        "15175a608b3cd1cb5f69395f60dc4055efbb9f9b07d09647f67c97c2a7d240fa"
    );

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--allow-read-write",
            &work.path().to_string_lossy(),
            "--require-write-sha256",
            &exact_requirement,
        ])
        .write_stdin("complete the exact artifact\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read(&required).unwrap(), b"launch contents\n");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().all(|event| {
        event["type"] != "run_started"
            && event["type"] != "provider_request"
            && event["type"] != "completion_postcondition"
    }));
    let message = events
        .iter()
        .find(|event| event["type"] == "error")
        .and_then(|event| event["message"].as_str())
        .expect("typed preflight error");
    assert!(
        message.contains("already has the required SHA-256"),
        "{message}"
    );
    assert!(message.contains("final content change"), "{message}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn test_required_write_sha256_conflicting_canonical_duplicates_fail_before_provider() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let work = TempDir::new().unwrap();
    let required = work.path().join("new-exact.txt");
    let aliased = work.path().join(".").join("new-exact.txt");
    let first = format!("{}={}", required.to_string_lossy(), "11".repeat(32));
    let second = format!("{}={}", aliased.to_string_lossy(), "22".repeat(32));

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write-sha256",
            &first,
            "--require-write-sha256",
            &second,
        ])
        .write_stdin("complete the exact artifact\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!required.exists());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().all(|event| {
        event["type"] != "run_started"
            && event["type"] != "provider_request"
            && event["type"] != "completion_postcondition"
    }));
    let message = events
        .iter()
        .find(|event| event["type"] == "error")
        .and_then(|event| event["message"].as_str())
        .expect("typed preflight error");
    assert!(
        message.contains("conflicting --require-write-sha256 expectations"),
        "{message}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn test_required_write_existing_read_write_target_succeeds() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let required = work.path().join("existing-result.txt");
    fs::write(&required, "launch contents\n").unwrap();
    let read_arguments = serde_json::json!({
        "path": required.to_string_lossy()
    })
    .to_string();
    let write_arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "verified replacement\n",
        "expected_previous_content": "launch contents\n"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"tool_call_id\":\"write_existing\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "TASK COMPLETE"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"tool_call_id\":\"read_existing\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "write_existing",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": write_arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(5)
        .expect(1)
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
                        "id": "read_existing",
                        "type": "function",
                        "function": {"name": "fs_read_file", "arguments": read_arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--allow-read-write",
            &work.path().to_string_lossy(),
            "--require-write",
            &required.to_string_lossy(),
        ])
        .write_stdin("read and safely replace the required result\n")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(&required).unwrap(),
        "verified replacement\n"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let evaluated = events
        .iter()
        .find(|event| event["type"] == "completion_postcondition" && event["phase"] == "evaluated")
        .expect("evaluated required-write receipt");
    assert_eq!(evaluated["status"], "satisfied");
    assert_eq!(evaluated["expectations"], serde_json::json!([]));
    assert_eq!(evaluated["results"][0]["journal_recorded"], true);
    assert_eq!(evaluated["results"][0]["final_state_changed"], true);
    assert!(evaluated["results"][0]["expected_sha256"].is_null());
    assert!(evaluated["results"][0]["expected_sha256_matches"].is_null());
    assert_eq!(events.last().unwrap()["status"], "ok");
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_required_write_structured_output_uses_tool_then_finalization_phases() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"}},"required":["status"],"additionalProperties":false}"#;
    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let required = work.path().join("structured-result.txt");
    let write_arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "verified structured effect\n"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("tool-action phase is complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": r#"{"status":"ok"}"#},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(
            "\"tool_call_id\":\"write_structured\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "provisional action result"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(5)
        .expect(1)
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
                        "id": "write_structured",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": write_arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let schema = home.path().join("answer.schema.json");
    let destination = home.path().join("last-message.json");
    fs::write(&schema, SCHEMA).unwrap();
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-schema",
            &schema.to_string_lossy(),
            "-o",
            &destination.to_string_lossy(),
            "chat",
            "--session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write",
            &required.to_string_lossy(),
        ])
        .write_stdin("create the required effect and return structured status\n")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(&required).unwrap(),
        "verified structured effect\n"
    );
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        r#"{"status":"ok"}"#
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let requests = events
        .iter()
        .filter(|event| event["type"] == "provider_request")
        .map(|event| {
            (
                event["tools_offered"].as_u64().unwrap(),
                event["structured_output"].as_bool().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].0 > 0);
    assert_eq!(requests[1].0, requests[0].0);
    assert_eq!(
        requests.iter().map(|value| value.1).collect::<Vec<_>>(),
        [false, false, true]
    );
    assert_eq!(requests[2].0, 0);
    assert!(events.iter().all(|event| {
        event["type"] != "assistant" || event["text"].as_str() != Some("provisional action result")
    }));
    assert_eq!(events.last().unwrap()["status"], "ok");
    assert_eq!(
        events.last().unwrap()["session_snapshot_state"],
        "resumable"
    );

    let session_id = events.last().unwrap()["session_id"]
        .as_str()
        .expect("durable structured-output session id");
    let mut list = base_cmd(home.path());
    let listed_output = list
        .args(["sessions", "--json"])
        .output()
        .expect("list structured-output session");
    assert!(listed_output.status.success(), "{listed_output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&listed_output.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["id"], session_id);
    assert_eq!(
        listed[0]["turns"], 1,
        "the provider-only finalization nudge is not a second durable user turn"
    );

    let mut show = base_cmd(home.path());
    let shown_output = show
        .args(["sessions", "--json", "show", session_id])
        .output()
        .expect("show structured-output session");
    assert!(shown_output.status.success(), "{shown_output:?}");
    let shown: serde_json::Value = serde_json::from_slice(&shown_output.stdout).unwrap();
    let saved_messages = shown["messages"].as_array().expect("saved transcript");
    assert!(saved_messages.iter().all(|message| {
        message["content"].as_str() != Some("provisional action result")
            && !message["content"]
                .as_str()
                .is_some_and(|content| content.contains("tool-action phase is complete"))
    }));
    assert!(saved_messages.iter().any(|message| {
        message["role"] == "assistant" && message["tool_calls"][0]["call_id"] == "write_structured"
    }));
    assert!(saved_messages.iter().any(|message| {
        message["role"] == "tool" && message["tool_call_id"] == "write_structured"
    }));
    assert_eq!(
        saved_messages.last().unwrap()["content"],
        r#"{"status":"ok"}"#
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let bodies = requests
        .iter()
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap())
        .collect::<Vec<_>>();
    assert!(bodies[0].get("response_format").is_none());
    assert!(bodies[1].get("response_format").is_none());
    assert_eq!(bodies[2]["response_format"]["type"], "json_schema");
    assert!(bodies[0]["tools"].is_array());
    assert!(bodies[1]["tools"].is_array());
    assert!(bodies[2].get("tools").is_none());
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_exact_write_mismatch_gets_bounded_tool_repair_before_finalization() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"}},"required":["status"],"additionalProperties":false}"#;
    const EXPECTED_SHA256: &str =
        "bdbb8f375955d2fdcd0792b830b135a7edab099e626a8d9078c13f9f062922df";
    const WRONG_SHA256: &str = "b435f9a7a4c3d6a12feb5bf83ed470aea19292b293769472c184cb14e7957167";
    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let required = work.path().join("repair-before-final.json");
    let wrong_arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "wrong repaired result\n"
    })
    .to_string();
    let corrected_arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "verified repaired result\n",
        "expected_previous_content": "wrong repaired result\n"
    })
    .to_string();

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("tool-action phase is complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": r#"{"status":"ok"}"#},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(
            "\"tool_call_id\":\"write_corrected_exact\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "provisional repaired result"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(3)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(
            "latest filesystem write does not yet match the required raw-byte SHA-256",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "write_corrected_exact",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": corrected_arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(5)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(
            "\"tool_call_id\":\"write_wrong_exact\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "provisional mismatched result"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(7)
        .expect(1)
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
                        "id": "write_wrong_exact",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": wrong_arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let schema = home.path().join("repair-answer.schema.json");
    let destination = home.path().join("last-message.json");
    fs::write(&schema, SCHEMA).unwrap();
    fs::write(&destination, "previous host output").unwrap();
    let requirement = format!("{}={EXPECTED_SHA256}", required.to_string_lossy());
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--max-rounds",
            "5",
            "--output-schema",
            &schema.to_string_lossy(),
            "-o",
            &destination.to_string_lossy(),
            "chat",
            "--session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write-sha256",
            &requirement,
        ])
        .write_stdin("create the exact required result and return structured status\n")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(fs::read(&required).unwrap(), b"verified repaired result\n");
    assert_eq!(fs::read(&destination).unwrap(), br#"{"status":"ok"}"#);
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let requests = events
        .iter()
        .filter(|event| event["type"] == "provider_request")
        .map(|event| {
            (
                event["tools_offered"].as_u64().unwrap(),
                event["structured_output"].as_bool().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 5, "events: {events:#?}");
    assert_eq!(
        requests.iter().map(|request| request.1).collect::<Vec<_>>(),
        [false, false, false, false, true]
    );
    assert!(requests[..4].iter().all(|request| request.0 > 0));
    assert_eq!(requests[4], (0, true));
    let deferred = events
        .iter()
        .find(|event| event["type"] == "structured_finalization_deferred")
        .expect("typed bounded-repair receipt");
    assert_eq!(deferred["round"], 2);
    assert_eq!(deferred["repair_round"], 3);
    assert_eq!(deferred["finalization_round_reserved"], true);
    assert_eq!(deferred["cause"], "required_write_sha256_mismatch");
    assert_eq!(deferred["repair_instruction_disclosed"], true);
    assert_eq!(deferred["write_authority_granted"], false);
    assert_eq!(
        deferred["targets"][0]["path"],
        required.to_string_lossy().as_ref()
    );
    assert_eq!(deferred["targets"][0]["expected_sha256"], EXPECTED_SHA256);
    assert_eq!(deferred["targets"][0]["actual_sha256"], WRONG_SHA256);
    let evaluated = events
        .iter()
        .find(|event| event["type"] == "completion_postcondition" && event["phase"] == "evaluated")
        .expect("satisfied exact postcondition");
    assert_eq!(evaluated["status"], "satisfied");
    assert_eq!(evaluated["results"][0]["expected_sha256_matches"], true);
    let deferred_index = events
        .iter()
        .position(|event| event["type"] == "structured_finalization_deferred")
        .unwrap();
    let repair_request_index = events
        .iter()
        .enumerate()
        .skip(deferred_index + 1)
        .find(|(_, event)| event["type"] == "provider_request")
        .map(|(index, _)| index)
        .unwrap();
    assert!(deferred_index < repair_request_index);
    assert_eq!(events.last().unwrap()["status"], "ok");

    let session_id = events.last().unwrap()["session_id"]
        .as_str()
        .expect("durable repaired session");
    let mut list = base_cmd(home.path());
    let listed_output = list.args(["sessions", "--json"]).output().unwrap();
    assert!(listed_output.status.success(), "{listed_output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&listed_output.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["turns"], 1);
    let mut show = base_cmd(home.path());
    let shown_output = show
        .args(["sessions", "--json", "show", session_id])
        .output()
        .unwrap();
    assert!(shown_output.status.success(), "{shown_output:?}");
    let shown: serde_json::Value = serde_json::from_slice(&shown_output.stdout).unwrap();
    let saved_messages = shown["messages"].as_array().expect("saved transcript");
    assert!(saved_messages.iter().all(|message| {
        let content = message["content"].as_str().unwrap_or_default();
        content != "provisional mismatched result"
            && content != "provisional repaired result"
            && !content.contains("latest filesystem write does not yet match")
            && !content.contains("tool-action phase is complete")
    }));
    assert!(saved_messages.iter().any(|message| {
        message["role"] == "assistant" && message["tool_calls"][0]["call_id"] == "write_wrong_exact"
    }));
    assert!(saved_messages.iter().any(|message| {
        message["role"] == "assistant"
            && message["tool_calls"][0]["call_id"] == "write_corrected_exact"
    }));
    assert_eq!(
        saved_messages.last().unwrap()["content"],
        r#"{"status":"ok"}"#
    );

    let bodies = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bodies.len(), 5);
    assert!(bodies[2].get("response_format").is_none());
    assert!(bodies[2]["tools"].is_array());
    assert!(bodies[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["role"] == "user"
                && message["content"].as_str().is_some_and(|content| {
                    content.contains(EXPECTED_SHA256) && content.contains(WRONG_SHA256)
                })
        }));
    assert!(bodies[4].get("tools").is_none());
    assert_eq!(bodies[4]["response_format"]["type"], "json_schema");
    assert!(bodies[4]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|message| {
            !message["content"].as_str().is_some_and(|content| {
                content.contains("latest filesystem write does not yet match")
            })
        }));
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_failed_structured_postcondition_session_excludes_finalization_scaffolding() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"}},"required":["status"],"additionalProperties":false}"#;
    const EXPECTED_SHA256: &str =
        "6decf6cca7939a61838a68c115c35fd10f664ed25012c99cb3af39058d9002ef";
    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let required = work.path().join("durable-mismatch.txt");
    let write_arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "wrong durable result\n"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("tool-action phase is complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": r#"{"status":"ok"}"#},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(
            "\"tool_call_id\":\"write_durable_mismatch\"",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "provisional failure handoff"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(5)
        .expect(1)
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
                        "id": "write_durable_mismatch",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": write_arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let schema = home.path().join("answer.schema.json");
    let destination = home.path().join("last-message.json");
    fs::write(&schema, SCHEMA).unwrap();
    fs::write(&destination, "preserved host output").unwrap();
    let requirement = format!("{}={EXPECTED_SHA256}", required.to_string_lossy());
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--max-rounds",
            "3",
            "--output-schema",
            &schema.to_string_lossy(),
            "-o",
            &destination.to_string_lossy(),
            "chat",
            "--session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write-sha256",
            &requirement,
        ])
        .write_stdin("write and finalize the durable result\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(fs::read(&required).unwrap(), b"wrong durable result\n");
    assert_eq!(fs::read(&destination).unwrap(), b"preserved host output");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let summary = events.last().expect("final summary");
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["postcondition_failures"], 1);
    assert_eq!(summary["session_snapshot_state"], "resumable");
    let session_id = summary["session_id"]
        .as_str()
        .expect("resumable failed-postcondition session");

    let mut list = base_cmd(home.path());
    let listed_output = list.args(["sessions", "--json"]).output().unwrap();
    assert!(listed_output.status.success(), "{listed_output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&listed_output.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["id"], session_id);
    assert_eq!(listed[0]["turns"], 1);

    let mut show = base_cmd(home.path());
    let shown_output = show
        .args(["sessions", "--json", "show", session_id])
        .output()
        .unwrap();
    assert!(shown_output.status.success(), "{shown_output:?}");
    let shown: serde_json::Value = serde_json::from_slice(&shown_output.stdout).unwrap();
    let saved_messages = shown["messages"].as_array().expect("saved transcript");
    assert!(saved_messages.iter().all(|message| {
        message["content"].as_str() != Some("provisional failure handoff")
            && !message["content"]
                .as_str()
                .is_some_and(|content| content.contains("tool-action phase is complete"))
    }));
    assert!(saved_messages.iter().any(|message| {
        message["role"] == "assistant"
            && message["tool_calls"][0]["call_id"] == "write_durable_mismatch"
    }));
    assert!(saved_messages.iter().any(|message| {
        message["role"] == "tool" && message["tool_call_id"] == "write_durable_mismatch"
    }));
    assert_eq!(
        saved_messages.last().unwrap()["content"],
        r#"{"status":"ok"}"#
    );
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_required_write_finalization_tool_call_is_typed_and_never_executed() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"}},"required":["status"],"additionalProperties":false}"#;
    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let required = work.path().join("retained-action-result.txt");
    let action_arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "verified action effect\n"
    })
    .to_string();
    let forbidden_final_arguments = serde_json::json!({
        "path": required.to_string_lossy(),
        "content": "forbidden final mutation\n",
        "expected_previous_content": "verified action effect\n"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("tool-action phase is complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "forbidden_final_write",
                        "type": "function",
                        "function": {
                            "name": "fs_write_file",
                            "arguments": forbidden_final_arguments
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"tool_call_id\":\"write_action\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": r#"{"status":"ok"}"#},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(5)
        .expect(1)
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
                        "id": "write_action",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": action_arguments}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let schema = home.path().join("answer.schema.json");
    let script = home.path().join("finalization-tool-call.script");
    let destination = home.path().join("last-message.json");
    fs::write(&schema, SCHEMA).unwrap();
    fs::write(&script, "create the required effect and return status\n").unwrap();
    fs::write(&destination, "preserved host output").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-schema",
            &schema.to_string_lossy(),
            "-o",
            &destination.to_string_lossy(),
            "chat",
            "--no-session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write",
            &required.to_string_lossy(),
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        fs::read_to_string(&required).unwrap(),
        "verified action effect\n",
        "the unoffered finalization call must not mutate the retained action effect"
    );
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "preserved host output"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let requests = events
        .iter()
        .filter(|event| event["type"] == "provider_request")
        .map(|event| {
            (
                event["tools_offered"].as_u64().unwrap(),
                event["structured_output"].as_bool().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].0 > 0);
    assert_eq!(requests[1].0, requests[0].0);
    assert_eq!(requests[2], (0, true));
    let tool_starts = events
        .iter()
        .filter(|event| event["type"] == "tool_started")
        .collect::<Vec<_>>();
    assert_eq!(tool_starts.len(), 1);
    assert_eq!(tool_starts[0]["call_id"], "write_action");
    assert!(events.iter().all(|event| {
        event["type"] != "assistant" || event["text"].as_str() != Some(r#"{"status":"ok"}"#)
    }));
    let error = events
        .iter()
        .find(|event| event["type"] == "error" && event["kind"] == "provider")
        .expect("typed provider-class finalization error");
    assert_eq!(error["cause"], "tool_call_during_tool_free_finalization");
    assert_eq!(
        error["message"],
        "model returned a tool call after tools were withheld for structured finalization; no tool was executed"
    );
    assert_eq!(error["exit_code"], 2);
    let evaluated = events
        .iter()
        .find(|event| event["type"] == "completion_postcondition" && event["phase"] == "evaluated")
        .expect("required-write effects remain independently auditable");
    assert_eq!(evaluated["status"], "satisfied");
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let summary = events.last().unwrap();
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["provider_failures"], 1);
    assert_eq!(summary["output_schema_failures"], 0);
    assert_eq!(summary["postcondition_failures"], 0);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_required_write_script_failure_is_typed_and_preserves_output() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","const":"ok"}},"required":["status"],"additionalProperties":false}"#;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": r#"{"status":"ok"}"#},
                "finish_reason": "stop"
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let work = TempDir::new().unwrap();
    let required = work.path().join("must-change.txt");
    fs::write(&required, "unchanged task output\n").unwrap();
    let schema = home.path().join("answer.schema.json");
    fs::write(&schema, SCHEMA).unwrap();
    let script = home.path().join("required-write.script");
    fs::write(&script, "complete the task and return valid JSON\n").unwrap();
    let destination = home.path().join("last-message.json");
    fs::write(&destination, "previous host output").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-schema",
            &schema.to_string_lossy(),
            "-o",
            &destination.to_string_lossy(),
            "chat",
            "--no-session",
            "--allow-turn-errors",
            "--allow-read-write",
            &work.path().to_string_lossy(),
            "--require-write",
            &required.to_string_lossy(),
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        fs::read_to_string(&required).unwrap(),
        "unchanged task output\n"
    );
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "previous host output"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let provider_requests = events
        .iter()
        .filter(|event| event["type"] == "provider_request")
        .map(|event| {
            (
                event["tools_offered"].as_u64().unwrap(),
                event["structured_output"].as_bool().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(provider_requests.len(), 2);
    assert!(provider_requests[0].0 > 0);
    assert!(!provider_requests[0].1);
    assert_eq!(provider_requests[1], (0, true));
    let disposition = events
        .iter()
        .position(|event| event["type"] == "assistant_disposition")
        .unwrap();
    assert_eq!(events[disposition]["status"], "committed");
    let turn_finished = events
        .iter()
        .position(|event| event["type"] == "turn_finished")
        .unwrap();
    let evaluated = events
        .iter()
        .position(|event| {
            event["type"] == "completion_postcondition" && event["phase"] == "evaluated"
        })
        .unwrap();
    assert!(disposition < turn_finished && turn_finished < evaluated);
    assert_eq!(events[evaluated]["status"], "failed");
    assert_eq!(events[evaluated]["results"][0]["satisfied"], false);
    assert_eq!(events[evaluated]["results"][0]["journal_recorded"], false);
    assert!(events[evaluated]["results"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("no verified model filesystem write"));
    let error = events
        .iter()
        .find(|event| event["type"] == "error" && event["kind"] == "postcondition")
        .expect("typed postcondition error");
    assert_eq!(error["condition"], "required_write");
    assert_eq!(error["exit_code"], 1);
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let summary = events.last().unwrap();
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["failures"], 1);
    assert_eq!(summary["provider_failures"], 0);
    assert_eq!(summary["output_schema_failures"], 0);
    assert_eq!(summary["postcondition_failures"], 1);
    assert_eq!(summary["user_failures"], 1);
    assert_eq!(summary["successful_operations"], 1);
    assert_eq!(
        summary["output_last_message_state"],
        "preserved_postcondition_failure"
    );
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_postcondition_failure_reports_absent_output_preserved_only_when_requested() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "committed answer"},
                "finish_reason": "stop"
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let work = TempDir::new().unwrap();
    let required = work.path().join("missing-required.txt");
    let absent_output = home.path().join("absent-last-message.txt");

    let requested = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "-o",
            &absent_output.to_string_lossy(),
            "chat",
            "--no-session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write",
            &required.to_string_lossy(),
        ])
        .write_stdin("finish without writing the required target\n")
        .output()
        .unwrap();
    assert_eq!(requested.status.code(), Some(1), "{requested:?}");
    assert!(requested.stderr.is_empty(), "{requested:?}");
    assert!(!absent_output.exists());
    let requested_events = String::from_utf8(requested.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(requested_events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let requested_summary = requested_events.last().expect("requested final summary");
    assert_eq!(requested_summary["status"], "error");
    assert_eq!(requested_summary["postcondition_failures"], 1);
    assert_eq!(requested_summary["successful_operations"], 1);
    assert_eq!(
        requested_summary["output_last_message_state"],
        "preserved_postcondition_failure"
    );

    let unrequested = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write",
            &required.to_string_lossy(),
        ])
        .write_stdin("finish without writing the required target\n")
        .output()
        .unwrap();
    assert_eq!(unrequested.status.code(), Some(1), "{unrequested:?}");
    assert!(unrequested.stderr.is_empty(), "{unrequested:?}");
    let unrequested_events = String::from_utf8(unrequested.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let unrequested_summary = unrequested_events
        .last()
        .expect("unrequested final summary");
    assert_eq!(unrequested_summary["postcondition_failures"], 1);
    assert_eq!(unrequested_summary["successful_operations"], 1);
    assert!(unrequested_summary
        .get("output_last_message_state")
        .is_none());
    assert!(!required.exists());
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_required_write_without_write_authority_fails_before_provider() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let work = TempDir::new().unwrap();
    let required = work.path().join("not-authorized.txt");
    fs::write(&required, "original").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "chat",
            "--no-session",
            "--require-write",
            &required.to_string_lossy(),
        ])
        .write_stdin("try to complete the task\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read_to_string(&required).unwrap(), "original");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().all(|event| {
        event["type"] != "run_started"
            && event["type"] != "provider_request"
            && event["type"] != "completion_postcondition"
    }));
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn test_required_write_structured_output_reserves_a_final_round_before_provider() {
    use wiremock::MockServer;

    const SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string"}},"required":["status"],"additionalProperties":false}"#;
    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let work = TempDir::new().unwrap();
    let required = work.path().join("new-result.txt");
    let schema = home.path().join("answer.schema.json");
    fs::write(&schema, SCHEMA).unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--max-rounds",
            "1",
            "--output-schema",
            &schema.to_string_lossy(),
            "chat",
            "--no-session",
            "--allow-write",
            &work.path().to_string_lossy(),
            "--require-write",
            &required.to_string_lossy(),
        ])
        .write_stdin("create the required result\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!required.exists());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().all(|event| {
        event["type"] != "run_started"
            && event["type"] != "provider_request"
            && event["type"] != "completion_postcondition"
    }));
    let message = events
        .iter()
        .find(|event| event["type"] == "error")
        .and_then(|event| event["message"].as_str())
        .expect("typed preflight error");
    assert!(message.contains("--max-rounds 2 or higher"), "{message}");
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn test_output_schema_rejects_command_root_visibility_before_provider() {
    use wiremock::MockServer;

    const SCHEMA: &str = r#"{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}"#;
    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let command_root = TempDir::new().unwrap();
    let schema = command_root.path().join("answer.schema.json");
    fs::write(&schema, SCHEMA).unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-schema",
            &schema.to_string_lossy(),
            "chat",
            "--no-session",
            "--enable-commands",
            "--command-root",
            &command_root.path().to_string_lossy(),
            "do not run anything",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read_to_string(&schema).unwrap(), SCHEMA);
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .unwrap()
                .contains("--output-schema file")
            && event["message"]
                .as_str()
                .unwrap()
                .contains("writable command root")
    }));
    assert!(events.iter().all(|event| {
        event["type"] != "run_started"
            && event["type"] != "output_schema"
            && event["type"] != "provider_request"
    }));
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Model commands end with total elapsed time in both human and automation
/// surfaces. The provider delay makes this an accuracy assertion rather than
/// merely checking that a numeric field exists.
#[tokio::test]
#[serial]
async fn test_model_command_timing_receipt_and_jsonl_summary() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(40))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "timed answer"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}
                })),
        )
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut human = base_cmd(&home_path);
        let output = human
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--no-stream", "-p", "time this"])
            .output()
            .expect("run timed human prompt");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "timed answer\n");
        let stderr = String::from_utf8(output.stderr).unwrap();
        let timing = stderr
            .lines()
            .last()
            .and_then(|line| line.strip_prefix("kitsu-timing: "))
            .expect("timing receipt is the terminal human record");
        let timing: serde_json::Value = serde_json::from_str(timing).unwrap();
        assert_eq!(timing["status"], "ok");
        assert!(timing["duration_ms"].as_u64().unwrap() >= 35, "{timing}");

        let mut jsonl = base_cmd(&home_path);
        let output = jsonl
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--jsonl", "--no-stream", "-p", "time this"])
            .output()
            .expect("run timed JSONL prompt");
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        let events = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let summary = events.last().expect("final summary");
        assert_eq!(summary["type"], "final_summary");
        assert_eq!(summary["status"], "ok");
        assert!(summary["duration_ms"].as_u64().unwrap() >= 35, "{summary}");

        let mut management = base_cmd(&home_path);
        let output = management
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .arg("doctor")
            .output()
            .expect("run provider-free management command");
        assert!(output.status.success(), "{output:?}");
        assert!(
            !String::from_utf8(output.stderr)
                .unwrap()
                .contains("kitsu-timing:"),
            "management commands must not gain a model-run receipt"
        );
    })
    .await
    .expect("timed prompt processes should join");

    server.verify().await;
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
    assert_eq!(events.last().unwrap()["successful_operations"], 2);
}

#[tokio::test]
#[serial]
async fn test_script_output_last_message_uses_the_last_committed_turn_once() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("second-output-marker"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "second committed"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "first committed"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("two-turns.script");
    fs::write(&script, "first-output-marker\nsecond-output-marker\n").unwrap();
    let last_message = home.path().join("last-message.txt");
    fs::write(&last_message, "stale").unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.clone();
    let last_message_for_run = last_message.clone();

    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--output-last-message",
                &last_message_for_run.to_string_lossy(),
                "chat",
                "--script",
                &script_path.to_string_lossy(),
                "--no-session",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(fs::read(&last_message).unwrap(), b"second committed");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let receipts = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event["type"] == "output_last_message")
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1, "events: {events:#?}");
    let (receipt_index, receipt) = receipts[0];
    assert!(receipt["turn_id"]
        .as_str()
        .unwrap()
        .ends_with("two-turns.script:2"));
    assert_eq!(receipt["bytes"], "second committed".len());
    assert_eq!(
        receipt["sha256"],
        "1b0cb82e06ef712077f1979f4263ac892c6e319bc0690ab018503f30e1bc61d2"
    );
    assert_eq!(receipt["model_write_authority"], false);
    let final_turn = events
        .iter()
        .rposition(|event| event["type"] == "turn_finished")
        .unwrap();
    assert!(final_turn < receipt_index && receipt_index < events.len() - 1);
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert!(events
        .last()
        .unwrap()
        .get("output_last_message_state")
        .is_none());
    server.verify().await;
}

#[test]
fn test_script_output_last_message_reports_preserved_without_committed_assistant() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("query-only-output.script");
    fs::write(&script, "/status\n/quit\n").unwrap();

    let existing = home.path().join("existing-last-message.txt");
    fs::write(&existing, "keep this host-owned value").unwrap();
    let mut jsonl = base_cmd(home.path());
    let output = jsonl
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--output-last-message",
            &existing.to_string_lossy(),
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("run query-only JSONL script");

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        fs::read_to_string(&existing).unwrap(),
        "keep this host-owned value"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let summary = events.last().expect("final summary");
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["status"], "ok");
    assert_eq!(
        summary["output_last_message_state"],
        "preserved_no_committed_assistant"
    );

    let absent = home.path().join("absent-last-message.txt");
    let mut human = base_cmd(home.path());
    human
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--output-last-message",
            &absent.to_string_lossy(),
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "~ --output-last-message unchanged: no committed assistant message",
        ))
        .stderr(predicate::str::contains(
            "~ run summary: 1 successful operation,",
        ))
        .stderr(predicate::str::contains("turn(s) ok").not());
    assert!(!absent.exists(), "a no-op must not create the destination");
}

#[tokio::test]
#[serial]
async fn test_script_output_last_message_keeps_earlier_commit_after_later_blank_turn() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(|request: &wiremock::Request| {
            let body = String::from_utf8_lossy(&request.body);
            let content = if body.contains("SECOND_BLANK_PROMPT") {
                " \n\t"
            } else {
                "first committed"
            };
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop"
                }]
            }))
        })
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("commit-then-blank.script");
    fs::write(&script, "FIRST_COMMIT_PROMPT\nSECOND_BLANK_PROMPT\n").unwrap();
    let destination = home.path().join("last-message.txt");
    fs::write(&destination, "older host output").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-last-message",
            &destination.to_string_lossy(),
            "chat",
            "--no-session",
            "--allow-turn-errors",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(&destination).unwrap(), b"first committed");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let receipts = events
        .iter()
        .filter(|event| event["type"] == "output_last_message")
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1, "events: {events:#?}");
    assert!(receipts[0]["turn_id"]
        .as_str()
        .unwrap()
        .ends_with("commit-then-blank.script:1"));
    assert_eq!(receipts[0]["bytes"], "first committed".len());
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["status"], "partial");
    assert_eq!(summary["failures"], 1);
    assert_eq!(summary["provider_failures"], 1);
    assert!(summary.get("output_last_message_state").is_none());
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_output_last_message_install_failure_is_typed_and_nonzero() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let output_dir = TempDir::new().unwrap();
    let destination = output_dir.path().join("last-message.txt");
    let displaced = output_dir.path().join("preflight-value.txt");
    fs::write(&destination, "host-owned previous value").unwrap();
    let destination_at_response = destination.clone();
    let displaced_at_response = displaced.clone();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_: &wiremock::Request| {
            fs::rename(&destination_at_response, &displaced_at_response).unwrap();
            fs::create_dir(&destination_at_response).unwrap();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {"role": "assistant", "content": "committed answer"},
                    "finish_reason": "stop"
                }]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-last-message",
            &destination.to_string_lossy(),
            "ask",
            "finish the task",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty());
    assert!(destination.is_dir());
    assert_eq!(
        fs::read_to_string(&displaced).unwrap(),
        "host-owned previous value"
    );
    assert!(fs::read_dir(output_dir.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".kitsu-")
    }));
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["kind"] == "output_last_message"
            && event["message"]
                .as_str()
                .unwrap()
                .contains("destination was left unchanged")
    }));
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    let summary = events.last().unwrap();
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["failures"], 1);
    assert_eq!(summary["successful_operations"], 1);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_output_last_message_destination_is_not_model_filesystem_authority() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let destination = work.path().join("last-message.txt");
    fs::write(&destination, "host-owned previous value").unwrap();
    let tool_args = serde_json::json!({
        "path": destination.to_string_lossy(),
        "content": "model attempted overwrite"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "provisional answer"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
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
                        "id": "overwrite_host_sink",
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

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("protected-output.script");
    fs::write(&script, "attempt the requested task\n").unwrap();
    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();
    let destination_for_run = destination.clone();
    let script_path = script.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--output-last-message",
                &destination_for_run.to_string_lossy(),
                "chat",
                "--no-session",
                "--allow-read-write",
                &work_path.to_string_lossy(),
                "--script",
                &script_path.to_string_lossy(),
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "host-owned previous value"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let tool = events
        .iter()
        .find(|event| event["type"] == "tool_finished")
        .expect("failed protected write receipt");
    assert_eq!(tool["success"], false);
    let tool_error: serde_json::Value =
        serde_json::from_str(tool["output"].as_str().unwrap()).unwrap();
    assert_eq!(tool_error["error"], "PolicyDenied");
    assert!(tool_error["detail"]
        .as_str()
        .unwrap()
        .contains("Kitsu-private"));
    assert!(events
        .iter()
        .all(|event| event["type"] != "output_last_message"));
    assert_eq!(events.last().unwrap()["policy_failures"], 1);
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_output_last_message_rejects_writable_command_authority_before_provider() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let work = TempDir::new().unwrap();
    let destination = work.path().join("last-message.txt");
    fs::write(&destination, "host-owned previous value").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-last-message",
            &destination.to_string_lossy(),
            "chat",
            "--no-session",
            "--enable-commands",
            "--command-root",
            &work.path().to_string_lossy(),
            "do not run anything",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "host-owned previous value"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .unwrap()
                .contains("writable command root")
    }));
    assert!(events.iter().all(|event| {
        event["type"] != "provider_request" && event["type"] != "output_last_message"
    }));
    assert_eq!(events.last().unwrap()["type"], "final_summary");
    assert_eq!(events.last().unwrap()["status"], "error");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
#[serial]
async fn test_output_last_message_rejects_readonly_command_runtime_authority_before_provider() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let runtime_root = TempDir::new().unwrap();
    let destination = runtime_root.path().join("last-message.txt");
    fs::write(&destination, "host-owned previous value").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-last-message",
            &destination.to_string_lossy(),
            "chat",
            "--no-session",
            "--command-runtime-root",
            &runtime_root.path().to_string_lossy(),
            "do not run anything",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "host-owned previous value"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .unwrap()
                .contains("read-only command runtime root")
    }));
    assert!(events
        .iter()
        .all(|event| event["type"] != "provider_request"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_output_last_message_rejects_command_root_hard_link_alias() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let command_root = TempDir::new().unwrap();
    let host = TempDir::new().unwrap();
    let destination = host.path().join("last-message.txt");
    let alias = command_root.path().join("command-visible-alias.txt");
    fs::write(&destination, "host-owned previous value").unwrap();
    fs::hard_link(&destination, &alias).unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "--no-stream",
            "--output-last-message",
            &destination.to_string_lossy(),
            "chat",
            "--no-session",
            "--enable-commands",
            "--command-root",
            &command_root.path().to_string_lossy(),
            "do not run anything",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        fs::read_to_string(&destination).unwrap(),
        "host-owned previous value"
    );
    assert_eq!(
        fs::read_to_string(&alias).unwrap(),
        "host-owned previous value"
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["message"]
                .as_str()
                .unwrap()
                .contains("multiple hard links")
    }));
    assert!(events
        .iter()
        .all(|event| event["type"] != "provider_request"));
    assert!(server.received_requests().await.unwrap().is_empty());
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

    let run_ids = tokio::task::spawn_blocking(move || {
        let mut run_ids = Vec::new();
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
            let summary = events.last().expect("final_summary");
            assert_eq!(summary["type"], "final_summary", "piped={piped}");
            assert_eq!(summary["tool_errors"], 0, "piped={piped}");
            assert_eq!(summary["policy_errors"], 0, "piped={piped}");
            assert_eq!(summary["recovered_tool_errors"], 0, "piped={piped}");
            assert_eq!(summary["recovered_policy_errors"], 0, "piped={piped}");
            let facts =
                jsonl_fixture::inspect_jsonl(&events, 2).expect("inspect provider-fault JSONL");
            assert_eq!(
                facts.class,
                jsonl_fixture::JsonlOutcomeClass::ProviderFault,
                "piped={piped}"
            );
            assert_eq!(facts.terminal_count, 1, "piped={piped}");
            assert_eq!(facts.provider_requests, 1, "piped={piped}");
            assert_eq!(facts.provider_attempts, 3, "piped={piped}");
            assert_eq!(facts.tool_started, 0, "piped={piped}");
            assert_eq!(facts.tool_finished, 0, "piped={piped}");
            run_ids.push(facts.run_id);
        }
        run_ids
    })
    .await
    .unwrap();
    assert_ne!(
        run_ids[0], run_ids[1],
        "separate runs need separate identity"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 6);
}

#[tokio::test]
#[serial]
async fn test_jsonl_preflight_error_keeps_stdout_machine_only() {
    let server = wiremock::MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
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
    assert_eq!(kinds, ["error", "final_summary"]);
    assert_eq!(events[0]["exit_code"], 1);
    assert!(events[0]["message"]
        .as_str()
        .is_some_and(|message| message.contains("refresh-token")));
    assert_eq!(events[1]["user_failures"], 1);
    let facts = jsonl_fixture::inspect_jsonl(&events, 1).expect("inspect preflight-fault JSONL");
    assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::UserFault);
    assert_eq!(facts.terminal_count, 1);
    assert_eq!(facts.provider_requests, 0);
    assert_eq!(facts.provider_attempts, 0);
    assert_eq!(facts.tool_started, 0);
    assert_eq!(facts.tool_finished, 0);
    assert!(server.received_requests().await.unwrap().is_empty());
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
    assert!(events[1]["session_id"].is_null());
    assert_eq!(events[1]["session_snapshot_state"], "disabled");
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

#[test]
fn test_help_exposes_screen_reader_mode() {
    let home = TempDir::new().unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--screen-reader"))
        .stdout(predicate::str::contains("KITSU_SCREEN_READER"));
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
    let facts = jsonl_fixture::inspect_jsonl(&events, 130).expect("inspect active cancellation");
    assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::Cancelled);
    assert_eq!(
        facts.assistant,
        jsonl_fixture::JsonlAssistantDisposition::Discarded
    );
    assert_eq!(facts.terminal_count, 1);
    assert_eq!(facts.provider_requests, 1);
    assert_eq!(facts.provider_attempts, 1);
    assert_eq!(facts.tool_started, 0);
    assert_eq!(facts.tool_finished, 0);
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
    let facts = jsonl_fixture::inspect_jsonl(&events, 130)
        .expect("inspect cancellation before piped input acceptance");
    assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::Cancelled);
    assert_eq!(
        facts.assistant,
        jsonl_fixture::JsonlAssistantDisposition::Absent
    );
    assert_eq!(facts.terminal_count, 1);
    assert_eq!(facts.provider_requests, 0);
    assert_eq!(facts.provider_attempts, 0);
    assert_eq!(facts.tool_started, 0);
    assert_eq!(facts.tool_finished, 0);
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
    let facts = jsonl_fixture::inspect_jsonl(&events, 130)
        .expect("inspect cancellation before script FIFO acceptance");
    assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::Cancelled);
    assert_eq!(
        facts.assistant,
        jsonl_fixture::JsonlAssistantDisposition::Absent
    );
    assert_eq!(facts.terminal_count, 1);
    assert_eq!(facts.provider_requests, 0);
    assert_eq!(facts.provider_attempts, 0);
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
    let facts =
        jsonl_fixture::inspect_jsonl(&events, 130).expect("inspect scripted command cancellation");
    assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::Cancelled);
    assert_eq!(
        facts.assistant,
        jsonl_fixture::JsonlAssistantDisposition::Discarded
    );
    assert_eq!(facts.terminal_count, 1);
    assert_eq!(facts.provider_requests, 1);
    assert_eq!(facts.provider_attempts, 1);
    assert_eq!(facts.tool_started, 0);
    assert_eq!(facts.tool_finished, 0);
}

/// EOF at the only authority-admission surface must mean denial, never an
/// implicit grant or a successful terminal inferred from input closure.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_interactive_confirmation_eof_denies_write_and_exposes_followup_send() {
    use std::io::Write as _;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let repo = TempDir::new().expect("temp interactive repo");
    let target = repo.path().join("must-not-exist.txt");
    let write_arguments = serde_json::json!({
        "path": target.to_string_lossy(),
        "content": "confirmation EOF must not write"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "CONFIRMATION_EOF_DENIED"
                },
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
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
                        "id": "confirmation-eof-write",
                        "type": "function",
                        "function": {
                            "name": "fs_write_file",
                            "arguments": write_arguments
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

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let (status, transcript) = tokio::task::spawn_blocking(move || {
        let mut prompt_sent = false;
        let mut eof_sent = false;
        let mut quit_sent = false;
        run_plain_interactive_chat_pty(&home_path, &repo_path, &[], |output, terminal| {
            let prompt_count = output
                .windows(b"kitsu>".len())
                .filter(|window| *window == b"kitsu>")
                .count();
            if !prompt_sent && prompt_count >= 1 {
                terminal
                    .write_all(b"request one guarded write\n")
                    .expect("submit guarded request");
                terminal.flush().unwrap();
                prompt_sent = true;
            }
            if prompt_sent
                && !eof_sent
                && output
                    .windows(b"permission> kitsu wants to write".len())
                    .any(|window| window == b"permission> kitsu wants to write")
            {
                // Ctrl+D at an empty canonical line makes read_line return EOF.
                terminal
                    .write_all(b"\x04")
                    .expect("submit confirmation EOF");
                terminal.flush().unwrap();
                eof_sent = true;
            }
            let final_seen = output
                .windows(b"CONFIRMATION_EOF_DENIED".len())
                .any(|window| window == b"CONFIRMATION_EOF_DENIED");
            if eof_sent && !quit_sent && final_seen && prompt_count >= 2 {
                terminal.write_all(b"/quit\n").expect("exit chat");
                terminal.flush().unwrap();
                quit_sent = true;
            }
        })
    })
    .await
    .expect("interactive confirmation fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        transcript.contains("permission> kitsu wants to write"),
        "{transcript}"
    );
    assert!(
        transcript.contains("CONFIRMATION_EOF_DENIED"),
        "{transcript}"
    );
    assert!(
        !transcript.contains("invalid permission response"),
        "EOF is a direct denial, not malformed consent: {transcript}"
    );
    assert!(!target.exists(), "confirmation EOF performed the write");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "current loop sends one denial receipt after confirmation EOF"
    );
    assert!(String::from_utf8_lossy(&requests[1].body).contains("\"role\":\"tool\""));
    server.verify().await;
}

/// Cancelling an active interactive run must clear already accepted type-ahead
/// before any queued prompt can become a provider request.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_interactive_quit_cancel_never_activates_queued_prompt() {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let request_seen = Arc::new(AtomicBool::new(false));
    let responder_seen = request_seen.clone();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_request: &wiremock::Request| {
            responder_seen.store(true, Ordering::Release);
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "must be cancelled"},
                        "finish_reason": "stop"
                    }]
                }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let repo = TempDir::new().expect("temp interactive repo");
    let repo_path = repo.path().to_path_buf();
    let driver_seen = request_seen.clone();
    let (status, transcript) = tokio::task::spawn_blocking(move || {
        let mut prompt_sent = false;
        let mut cancel_sent = false;
        run_plain_interactive_chat_pty(
            &home_path,
            &repo_path,
            &["--no-tools"],
            |output, terminal| {
                let prompt_visible = output
                    .windows(b"kitsu>".len())
                    .any(|window| window == b"kitsu>");
                if !prompt_sent && prompt_visible {
                    terminal
                        .write_all(b"ACTIVE_PROVIDER_REQUEST\n")
                        .expect("submit active prompt");
                    terminal.flush().unwrap();
                    prompt_sent = true;
                }
                if prompt_sent && !cancel_sent && driver_seen.load(Ordering::Acquire) {
                    terminal
                        .write_all(b"NEVER_ACTIVATE_QUEUED_PROMPT\n/quit cancel\n")
                        .expect("queue then cancel");
                    terminal.flush().unwrap();
                    cancel_sent = true;
                }
            },
        )
    })
    .await
    .expect("interactive queued-cancel fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        transcript.contains("exit cancellation completed"),
        "{transcript}"
    );
    assert!(
        transcript.contains("1 queued prompt(s) cleared"),
        "{transcript}"
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "queued work reached the provider");
    let request = String::from_utf8_lossy(&requests[0].body);
    assert!(request.contains("ACTIVE_PROVIDER_REQUEST"), "{request}");
    assert!(
        !request.contains("NEVER_ACTIVATE_QUEUED_PROMPT"),
        "{request}"
    );
    server.verify().await;
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

        let mut jsonl = base_cmd(&home_path);
        let output = jsonl
            .args(["--jsonl", "models"])
            .output()
            .expect("run machine-readable catalog");
        assert!(output.status.success(), "{output:?}");
        let events = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let catalog = events
            .iter()
            .find(|event| event["type"] == "model_catalog")
            .expect("model_catalog event");
        assert_eq!(catalog["count"], 2);
        assert_eq!(catalog["models"], serde_json::json!(["model-a", "model-b"]));
        assert_eq!(
            events
                .iter()
                .map(|event| event["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "run_started",
                "provider_request",
                "provider_attempt",
                "model_catalog",
                "final_summary",
            ]
        );
        assert_eq!(events[1]["operation_id"], "models:1");
        assert_eq!(events[1]["purpose"], "model_catalog");
        assert_eq!(events[1]["request_timeout_ms"], 120_000);
        assert_eq!(events[1]["turn_remaining_ms"], serde_json::Value::Null);
        assert_eq!(events[2]["operation_id"], "models:1");
        assert_eq!(events[2]["purpose"], "model_catalog");
        assert_eq!(events[2]["deadline_scope"], "provider_request");
        assert_eq!(events[2]["turn_remaining_ms"], serde_json::Value::Null);
        assert_eq!(events[2]["category"], "success");
        assert_eq!(events[2]["status"], 200);
        assert_eq!(events[2]["outcome"], "body_completed");
        assert_eq!(events[2]["terminal"], true);
        assert_eq!(events.last().unwrap()["successful_operations"], 1);
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
        assert_eq!(
            events
                .iter()
                .map(|event| event["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "run_started",
                "provider_request",
                "provider_attempt",
                "error",
                "final_summary",
            ]
        );
        assert_eq!(events[1]["operation_id"], "models:1");
        assert_eq!(events[1]["purpose"], "model_catalog");
        assert_eq!(events[2]["operation_id"], "models:1");
        assert_eq!(events[2]["purpose"], "model_catalog");
        assert_eq!(events[2]["attempt"], 1);
        assert_eq!(events[2]["max_attempts"], 1);
        assert_eq!(events[2]["category"], "unavailable");
        assert_eq!(events[2]["status"], 503);
        assert_eq!(events[2]["outcome"], "rejected");
        assert_eq!(events[2]["terminal"], true);
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
    assert_eq!(events.len(), 5, "events: {events:#?}");
    assert_eq!(events[0]["type"], "run_started");
    assert_eq!(events[1]["type"], "provider_request");
    assert_eq!(events[1]["operation_id"], "models:1");
    assert_eq!(events[1]["purpose"], "model_catalog");
    assert_eq!(events[2]["type"], "provider_attempt");
    assert_eq!(events[2]["operation_id"], "models:1");
    assert_eq!(events[2]["purpose"], "model_catalog");
    assert_eq!(events[2]["category"], "cancelled");
    assert_eq!(events[2]["status"], serde_json::Value::Null);
    assert_eq!(events[2]["outcome"], "cancelled");
    assert_eq!(events[2]["terminal"], true);
    assert_eq!(events[3]["kind"], "cancelled");
    assert_eq!(events[3]["exit_code"], 130);
    assert_eq!(events[4]["type"], "final_summary");
    assert_eq!(events[4]["status"], "cancelled");
    assert_eq!(events[4]["cancelled_failures"], 1);
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
    let last_message = home.path().join("piped-last-message.txt");
    let last_message_for_run = last_message.clone();

    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        cmd.args([
            "--output-last-message",
            &last_message_for_run.to_string_lossy(),
            "chat",
            "--gateway",
            "example",
        ]);
        cmd.write_stdin("hello\n"); // piped → non-TTY → one-shot
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("hi from chat"))
            .stdout(predicate::str::contains(TOKEN).not())
            .stderr(predicate::str::contains(
                "saved last committed assistant message",
            ));
    })
    .await
    .expect("spawned kitsu chat should join");
    assert_eq!(fs::read(&last_message).unwrap(), b"hi from chat");
}

/// Bare `kitsu` is chat, `-p` is the one-shot shorthand, environment selectors
/// beat config defaults, explicit flags beat the environment, and `/model`
/// remains the final in-session override. This wire-level test protects the
/// complete user-facing precedence chain rather than only clap field values.
#[tokio::test]
#[serial]
async fn test_default_chat_prompt_flag_and_environment_selection_precedence() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "selection ok"},
                "finish_reason": "stop"
            }]
        })))
        .expect(4)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "configured"

[gateways.configured]
flavor = "openai"
base_url = "{}"
chat_path = "/configured/chat"
models_path = ""
model = "configured-model"
auth = {{ method = "none" }}

[gateways.environment]
flavor = "openai"
base_url = "{}"
chat_path = "/environment/chat"
models_path = ""
model = "environment-profile-model"
auth = {{ method = "none" }}

[gateways.explicit]
flavor = "openai"
base_url = "{}"
chat_path = "/explicit/chat"
models_path = ""
model = "explicit-profile-model"
auth = {{ method = "none" }}
"#,
            server.uri(),
            server.uri(),
            server.uri()
        ),
    )
    .unwrap();
    let script = home.path().join("environment-model-override.txt");
    fs::write(&script, "/model slash-model\nslash override prompt\n").unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().into_owned();

    tokio::task::spawn_blocking(move || {
        let mut bare = base_cmd(&home_path);
        bare.env("KITSU_GATEWAY", "environment")
            .env("KITSU_MODEL", "environment-model")
            .arg("--no-stream")
            .write_stdin("bare default chat prompt\n");
        bare.assert()
            .success()
            .stdout(predicate::str::contains("selection ok"));

        let mut prompt = base_cmd(&home_path);
        prompt
            .env("KITSU_GATEWAY", "environment")
            .env("KITSU_MODEL", "environment-model")
            .args(["--no-stream", "-p", "prompt flag input"]);
        prompt
            .assert()
            .success()
            .stdout(predicate::str::contains("selection ok"));

        let mut explicit = base_cmd(&home_path);
        explicit
            .env("KITSU_GATEWAY", "environment")
            .env("KITSU_MODEL", "environment-model")
            .args([
                "--no-stream",
                "--gateway",
                "explicit",
                "--model",
                "explicit-model",
                "-p",
                "explicit flag input",
            ]);
        explicit
            .assert()
            .success()
            .stdout(predicate::str::contains("selection ok"));

        let mut slash = base_cmd(&home_path);
        slash
            .env("KITSU_GATEWAY", "environment")
            .env("KITSU_MODEL", "environment-model")
            .args(["--no-stream", "chat", "--script", &script_path]);
        slash
            .assert()
            .success()
            .stdout(predicate::str::contains("selection ok"))
            .stderr(predicate::str::contains("switched to slash-model"));
    })
    .await
    .expect("selection commands should join");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    let mut observed = std::collections::HashMap::new();
    for request in requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let prompt = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|message| message["role"] == "user")
            .and_then(|message| message["content"].as_str())
            .unwrap()
            .to_string();
        observed.insert(
            prompt,
            (
                request.url.path().to_string(),
                body["model"].as_str().unwrap().to_string(),
            ),
        );
    }
    assert_eq!(
        observed.get("bare default chat prompt"),
        Some(&(
            "/environment/chat".to_string(),
            "environment-model".to_string()
        ))
    );
    assert_eq!(
        observed.get("prompt flag input"),
        Some(&(
            "/environment/chat".to_string(),
            "environment-model".to_string()
        ))
    );
    assert_eq!(
        observed.get("explicit flag input"),
        Some(&("/explicit/chat".to_string(), "explicit-model".to_string()))
    );
    assert_eq!(
        observed.get("slash override prompt"),
        Some(&("/environment/chat".to_string(), "slash-model".to_string()))
    );
    assert!(
        observed
            .values()
            .all(|(path, _)| path != "/configured/chat"),
        "environment selection did not override the configured default: {observed:?}"
    );
    server.verify().await;
}

/// Every logical inference request identifies the exact model sent on the
/// wire. This remains truthful across an in-session `/model` switch; the
/// intervening catalog lookup is not routed to a model and omits the field.
#[tokio::test]
#[serial]
async fn test_jsonl_provider_requests_identify_model_after_in_session_switch() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "model identity ok"},
                "finish_reason": "stop"
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("model-request-identity.script");
    fs::write(
        &script,
        "first model prompt\n/model switched-model\nsecond model prompt\n/quit\n",
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().into_owned();

    let output = tokio::task::spawn_blocking(move || {
        base_cmd(&home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--gateway",
                "example",
                "--jsonl",
                "--no-stream",
                "chat",
                "--no-session",
                "--no-tools",
                "--script",
                &script_path,
            ])
            .output()
            .expect("run model-switch JSONL script")
    })
    .await
    .expect("model-switch command should join");

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let inference_models = events
        .iter()
        .filter(|event| event["type"] == "provider_request" && event["purpose"] == "inference")
        .map(|event| {
            event["model"]
                .as_str()
                .expect("inference request model")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(inference_models, ["example-model-v1", "switched-model"]);

    let catalog_request = events
        .iter()
        .find(|event| event["type"] == "provider_request" && event["purpose"] == "model_catalog")
        .expect("logical model-catalog request");
    assert!(
        catalog_request.get("model").is_none(),
        "catalog lookup is not routed to a model: {catalog_request}"
    );

    let requests = server.received_requests().await.unwrap();
    let wire_models = requests
        .iter()
        .map(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["model"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(wire_models, inference_models);
    server.verify().await;
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

/// A scripted turn gets one inference deadline across every provider/tool
/// round. Two individually-fast rounds whose combined delay exceeds the budget
/// must time out instead of receiving a fresh `--timeout-secs` allowance after
/// the tool call.
#[tokio::test]
#[serial]
async fn test_script_turn_timeout_is_shared_across_tool_rounds() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().expect("work dir");
    let file = work.path().join("deadline.txt");
    fs::write(&file, "deadline marker").expect("write fixture");
    let tool_args = serde_json::json!({"path": file}).to_string();

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(700))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "deadline_read_1",
                                "type": "function",
                                "function": {
                                    "name": "fs_read_file",
                                    "arguments": tool_args
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                })),
        )
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(700))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "too late"},
                        "finish_reason": "stop"
                    }]
                })),
        )
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("deadline.script");
    fs::write(&script, "read the deadline fixture\n").unwrap();
    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--timeout-secs",
                "1",
                "chat",
                "--no-session",
                "--allow-path",
                &work_path.to_string_lossy(),
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("run bounded scripted tool turn")
    })
    .await
    .expect("scripted deadline process should join");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stderr.is_empty(),
        "JSONL stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let provider_requests = events
        .iter()
        .filter(|event| event["type"] == "provider_request")
        .collect::<Vec<_>>();
    assert_eq!(provider_requests.len(), 2);
    assert!(provider_requests
        .iter()
        .all(|event| event["request_timeout_ms"] == 1_000));
    let turn_at_request = provider_requests
        .iter()
        .map(|event| event["turn_remaining_ms"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert!(turn_at_request[0] <= 1_000);
    assert!(turn_at_request[1] < turn_at_request[0]);
    let provider_attempts = events
        .iter()
        .filter(|event| event["type"] == "provider_attempt")
        .collect::<Vec<_>>();
    assert_eq!(provider_attempts.len(), 2);
    assert!(provider_attempts
        .iter()
        .all(|event| event["deadline_scope"] == "provider_request"));
    assert!(provider_attempts
        .iter()
        .all(|event| event["turn_remaining_ms"].is_u64()));
    let cancelled_attempt = provider_attempts.last().unwrap();
    assert_eq!(cancelled_attempt["category"], "cancelled");
    assert_eq!(cancelled_attempt["outcome"], "cancelled");
    assert!(cancelled_attempt["turn_remaining_ms"].as_u64().unwrap() <= 100);
    assert!(
        cancelled_attempt["remaining_deadline_ms"].as_u64().unwrap()
            > cancelled_attempt["turn_remaining_ms"].as_u64().unwrap()
    );
    assert!(events.iter().any(|event| {
        event["type"] == "tool_finished"
            && event["name"] == "fs_read_file"
            && event["success"] == true
    }));
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["kind"] == "provider"
            && event["message"]
                .as_str()
                .is_some_and(|message| message.contains("timed out after 1s"))
    }));
    assert!(events.iter().any(|event| {
        event["type"] == "assistant_disposition" && event["status"] == "discarded"
    }));
    let summary = events.last().expect("final summary");
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["provider_failures"], 1);
    server.verify().await;
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
async fn test_zero_spawn_cap_unregisters_agent_tools_from_slash_and_provider() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "DONE"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let work = TempDir::new().expect("tool root");
    fs::write(work.path().join("note.txt"), "fixture\n").unwrap();
    let script = home.path().join("zero-spawn-cap.script");
    fs::write(&script, "/tools\nReply with DONE without calling a tool.\n").unwrap();
    let home_path = home.path().to_path_buf();
    let work_path = work.path().to_path_buf();

    let events = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        let output = cmd
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--no-project-context",
                "--no-workspace-evidence",
                "--agent-spawn-cap",
                "0",
                "chat",
                "--no-session",
                "--allow-read-write",
                &work_path.to_string_lossy(),
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("run cap-zero chat");
        assert!(
            output.status.success(),
            "stderr: {}\nstdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(output.stderr.is_empty());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    })
    .await
    .expect("cap-zero chat process should join");

    let tools_result = events
        .iter()
        .find(|event| event["type"] == "command_result" && event["result"]["kind"] == "tools")
        .expect("structured /tools result");
    let slash_tools = tools_result["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(slash_tools.iter().any(|name| name == "fs_read_file"));
    assert!(slash_tools.iter().any(|name| name == "fs_write_file"));
    for disabled in [
        "spawn_agent",
        "spawn_agents",
        "spawn_agent_bg",
        "agent_result",
    ] {
        assert!(
            !slash_tools.iter().any(|name| name == disabled),
            "cap-zero /tools advertised {disabled}: {slash_tools:?}"
        );
    }
    let provider_request = events
        .iter()
        .find(|event| event["type"] == "provider_request")
        .expect("provider request event");
    assert_eq!(
        provider_request["tools_offered"].as_u64().unwrap() as usize,
        slash_tools.len()
    );

    let requests = server.received_requests().await.expect("record request");
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let offered = body["tools"]
        .as_array()
        .expect("tool schemas")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(offered, slash_tools);
    let system_prompt = body["messages"][0]["content"]
        .as_str()
        .expect("system prompt");
    for disabled in [
        "spawn_agent",
        "spawn_agents",
        "spawn_agent_bg",
        "agent_result",
    ] {
        assert!(
            !system_prompt.contains(disabled),
            "cap-zero system prompt advertised {disabled}: {system_prompt}"
        );
    }
    server.verify().await;
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
            .stderr(predicate::str::contains("script summary: 1 failure\n"))
            .stderr(predicate::str::contains("failure(s)").not())
            .stderr(predicate::str::contains("\"status\":\"partial\""));
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

        let mut jsonl = base_cmd(&home_path);
        let output = jsonl
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--gateway",
                "example",
                "--jsonl",
                "--no-stream",
                "chat",
                "--allow-path",
                &allowed.to_string_lossy(),
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("run recovered JSONL turn");
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        let events = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let turn = events
            .iter()
            .find(|event| event["type"] == "turn_finished")
            .expect("turn_finished");
        assert_eq!(turn["status"], "ok");
        assert_eq!(turn["tool_errors"], 1);
        assert_eq!(turn["policy_errors"], 0);
        let summary = events.last().expect("final_summary");
        assert_eq!(summary["type"], "final_summary");
        assert_eq!(summary["status"], "ok");
        assert_eq!(summary["failures"], 0);
        assert_eq!(summary["tool_failures"], 0);
        assert_eq!(summary["policy_failures"], 0);
        assert_eq!(summary["tool_errors"], 1);
        assert_eq!(summary["policy_errors"], 0);
        assert_eq!(summary["recovered_tool_errors"], 1);
        assert_eq!(summary["recovered_policy_errors"], 0);

        let mut piped = base_cmd(&home_path);
        let output = piped
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--gateway",
                "example",
                "--jsonl",
                "--no-stream",
                "chat",
                "--no-session",
                "--allow-path",
                &allowed.to_string_lossy(),
            ])
            .write_stdin("read the data file\n")
            .output()
            .expect("run recovered piped JSONL turn");
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        let summary = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .next_back()
            .expect("piped final_summary");
        assert_eq!(summary["type"], "final_summary");
        assert_eq!(summary["status"], "ok");
        assert_eq!(summary["failures"], 0);
        assert_eq!(summary["tool_failures"], 0);
        assert_eq!(summary["tool_errors"], 1);
        assert_eq!(summary["recovered_tool_errors"], 1);
        assert_eq!(summary["policy_errors"], 0);
        assert_eq!(summary["recovered_policy_errors"], 0);
    })
    .await
    .expect("join");
}

/// Strict automation must not turn a blank terminal model message into a
/// successful recovery. This is especially dangerous after earlier tool work:
/// the process would otherwise exit zero even though it produced no completion
/// and never proved the requested outcome.
#[tokio::test]
#[serial]
async fn test_scripted_empty_final_response_after_tool_activity_is_provider_failure() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("strict-empty-marker"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "   \n\t"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;

    let data = TempDir::new().unwrap();
    let data_path = data.path().join("data.txt");
    fs::write(&data_path, "strict-empty-marker").unwrap();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "read_ok",
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
        .expect(1)
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
                        "id": "bad_first",
                        "type": "function",
                        "function": {"name": "fs_read_file", "arguments": "{}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let script = home.path().join("empty-final.script");
    fs::write(&script, "recover and answer\n").unwrap();
    let last_message = home.path().join("last-message.txt");
    fs::write(&last_message, "keep previous committed answer").unwrap();
    let last_message_for_run = last_message.clone();
    let home_path = home.path().to_path_buf();
    let allowed = data.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--gateway",
                "example",
                "--jsonl",
                "--no-stream",
                "--output-last-message",
                &last_message_for_run.to_string_lossy(),
                "chat",
                "--session",
                "--allow-path",
                &allowed.to_string_lossy(),
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("run strict empty-response script")
    })
    .await
    .expect("join");

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let disposition = events
        .iter()
        .find(|event| event["type"] == "assistant_disposition")
        .expect("assistant disposition");
    assert_eq!(disposition["status"], "discarded");
    assert!(disposition["reason"]
        .as_str()
        .unwrap()
        .contains("no final assistant text"));
    let error = events
        .iter()
        .find(|event| event["type"] == "error")
        .expect("typed error");
    assert_eq!(error["kind"], "incomplete_turn");
    assert_eq!(error["exit_code"], 2);
    assert_eq!(error["tool_errors"], 1);
    assert_eq!(error["policy_errors"], 0);
    let summary = events.last().expect("final summary");
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["failures"], 1);
    assert_eq!(summary["provider_failures"], 1);
    assert_eq!(summary["tool_failures"], 0);
    assert_eq!(summary["tool_errors"], 1);
    assert_eq!(summary["recovered_tool_errors"], 0);
    assert!(summary["session_id"].is_null());
    assert_eq!(
        summary["session_snapshot_state"],
        "pending_until_first_success"
    );
    assert_eq!(
        summary["output_last_message_state"],
        "preserved_no_committed_assistant"
    );
    assert!(
        events
            .iter()
            .all(|event| event["type"] != "output_last_message"),
        "discarded assistant text must never produce an output receipt"
    );
    assert_eq!(
        fs::read_to_string(&last_message).unwrap(),
        "keep previous committed answer"
    );

    let mut sessions = base_cmd(home.path());
    let listed = sessions
        .args(["sessions", "--json"])
        .output()
        .expect("list sessions after failed fresh durable turn");
    assert!(listed.status.success(), "{listed:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&listed.stdout).unwrap(),
        serde_json::json!([])
    );
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_failed_resumed_turn_keeps_resumable_terminal_session_receipt() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SAVED_MARKER: &str = "SESSION_RECEIPT_SAVED_731";
    const FAILED_MARKER: &str = "SESSION_RECEIPT_FAILED_947";

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(FAILED_MARKER))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "  \n\t"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "saved receipt answer"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let create_script = home.path().join("receipt-create.script");
    let fail_script = home.path().join("receipt-fail.script");
    fs::write(&create_script, format!("remember {SAVED_MARKER}\n")).unwrap();
    fs::write(&fail_script, format!("do not retain {FAILED_MARKER}\n")).unwrap();
    let home_path = home.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let run = |resume: Option<&str>, script: &Path| {
            let mut command = base_cmd(&home_path);
            command.env("ANTHROPIC_AUTH_TOKEN", TOKEN).args([
                "--gateway",
                "example",
                "--jsonl",
                "--no-stream",
                "chat",
            ]);
            if let Some(id) = resume {
                command.args(["--resume", id]);
            } else {
                command.arg("--session");
            }
            command
                .args(["--no-tools", "--script", &script.to_string_lossy()])
                .output()
                .expect("run session-receipt lifecycle")
        };

        let created = run(None, &create_script);
        assert!(created.status.success(), "{created:?}");
        assert!(created.stderr.is_empty(), "{created:?}");
        let created_events = String::from_utf8(created.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let created_summary = created_events.last().expect("created final_summary");
        let id = created_summary["session_id"]
            .as_str()
            .expect("materialized session id")
            .to_string();
        assert_eq!(created_summary["session_snapshot_state"], "resumable");

        let failed = run(Some(&id), &fail_script);
        assert_eq!(failed.status.code(), Some(2), "{failed:?}");
        assert!(failed.stderr.is_empty(), "{failed:?}");
        let failed_events = String::from_utf8(failed.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let started = failed_events
            .iter()
            .find(|event| event["type"] == "run_started")
            .expect("resumed run_started");
        assert_eq!(started["session_id"], id);
        assert_eq!(started["session_snapshot_state"], "resumable");
        let summary = failed_events.last().expect("failed final_summary");
        assert_eq!(summary["status"], "error");
        assert_eq!(summary["session_id"], id);
        assert_eq!(summary["session_snapshot_state"], "resumable");

        let mut show = base_cmd(&home_path);
        let output = show
            .args(["sessions", "--json", "show", &id])
            .output()
            .expect("show preserved session");
        assert!(output.status.success(), "{output:?}");
        let saved: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let transcript = saved["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(transcript.contains(SAVED_MARKER), "{transcript}");
        assert!(!transcript.contains(FAILED_MARKER), "{transcript}");
    })
    .await
    .expect("join session-receipt lifecycle");

    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_empty_final_response_is_provider_failure_for_ask_and_piped_chat() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": ""},
                "finish_reason": "stop"
            }]
        })))
        .expect(2)
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
                cmd.args([
                    "--gateway",
                    "example",
                    "--jsonl",
                    "--no-stream",
                    "chat",
                    "--no-session",
                    "--no-tools",
                ])
                .write_stdin("answer this\n");
            } else {
                cmd.args([
                    "--gateway",
                    "example",
                    "--jsonl",
                    "--no-stream",
                    "ask",
                    "answer this",
                ]);
            }
            let output = cmd.output().expect("run empty-response mode");
            assert_eq!(output.status.code(), Some(2), "piped={piped}: {output:?}");
            assert!(
                output.stderr.is_empty(),
                "piped={piped}: {:?}",
                output.stderr
            );
            let events = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            let disposition = events
                .iter()
                .find(|event| event["type"] == "assistant_disposition")
                .expect("assistant disposition");
            assert_eq!(disposition["status"], "discarded", "piped={piped}");
            let error = events
                .iter()
                .find(|event| event["type"] == "error")
                .expect("typed incomplete-turn error");
            assert_eq!(error["kind"], "incomplete_turn", "piped={piped}");
            assert_eq!(error["exit_code"], 2, "piped={piped}");
            let summary = events.last().expect("final summary");
            assert_eq!(summary["status"], "error", "piped={piped}");
            assert_eq!(summary["provider_failures"], 1, "piped={piped}");
            assert_eq!(summary["tool_errors"], 0, "piped={piped}");
            assert_eq!(summary["recovered_tool_errors"], 0, "piped={piped}");
        }
    })
    .await
    .expect("join");
    server.verify().await;
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_interactive_empty_response_rolls_back_and_keeps_retry_available() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": " \n\t"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();

    let transcript = tokio::task::spawn_blocking(move || {
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: both output pointers are valid; null termios and winsize
        // request the platform defaults.
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
            "allocate interactive empty-response PTY"
        );
        // SAFETY: `master` is live; changing descriptor flags preserves its
        // ownership and lets this test synchronize without blocking forever.
        let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        // SAFETY: each duplicated slave descriptor is transferred to exactly
        // one Stdio owner below.
        let child_stdin = unsafe { libc::dup(slave) };
        let child_stdout = unsafe { libc::dup(slave) };
        assert!(child_stdin >= 0 && child_stdout >= 0);

        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
        cmd.env("HOME", &home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .env("KITSU_NO_PROJECT_CONTEXT", "1")
            .env_remove("ANTHROPIC_TOKEN_EXPIRES")
            // SAFETY: the duplicated descriptors are valid and transferred
            // exactly once. Ownership of the original slave transfers to
            // stderr.
            .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
            .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
            .stderr(unsafe { Stdio::from_raw_fd(slave) })
            .args([
                "--plain",
                "--gateway",
                "example",
                "--no-stream",
                "chat",
                "--session",
                "--no-tools",
            ]);
        let mut child = cmd.spawn().expect("spawn interactive empty-response chat");
        // SAFETY: `master` is the remaining live descriptor and transfers to
        // this File exactly once.
        let mut terminal = unsafe { fs::File::from_raw_fd(master) };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut prompt_sent = false;
        let mut quit_sent = false;
        let marker = b"turn rolled back - /retry to try again";

        let status = loop {
            let mut buffer = [0_u8; 4096];
            match terminal.read(&mut buffer) {
                Ok(0) => {}
                Ok(read) => output.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
                Err(error) => panic!("read interactive empty-response PTY: {error}"),
            }
            let prompt_count = output
                .windows(b"kitsu>".len())
                .filter(|window| *window == b"kitsu>")
                .count();
            if !prompt_sent && prompt_count >= 1 {
                terminal.write_all(b"answer this\n").unwrap();
                terminal.flush().unwrap();
                prompt_sent = true;
            }
            let saw_rollback = output.windows(marker.len()).any(|window| window == marker);
            if prompt_sent && !quit_sent && saw_rollback && prompt_count >= 2 {
                terminal.write_all(b"/quit\n").unwrap();
                terminal.flush().unwrap();
                quit_sent = true;
            }
            if let Some(status) = child.try_wait().expect("poll interactive empty response") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "interactive empty-response chat did not exit: {}",
                    String::from_utf8_lossy(&output)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "interactive child failed: {status}");
        assert!(
            prompt_sent && quit_sent,
            "incomplete PTY handshake: {output:?}"
        );
        String::from_utf8_lossy(&output).into_owned()
    })
    .await
    .expect("interactive PTY task joins");

    assert!(transcript.contains("model returned no final assistant text"));
    assert!(transcript.contains("turn rolled back - /retry to try again"));
    let mut sessions = base_cmd(home.path());
    let output = sessions
        .args(["sessions", "--json"])
        .output()
        .expect("list sessions after blank interactive turn");
    assert!(output.status.success(), "{output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed, serde_json::json!([]));
    server.verify().await;
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
                "strict scripted turn failed: model output hit the gateway's length limit",
            ))
            .stderr(predicate::str::contains(
                "retry with an explicit larger --max-output-tokens <n>",
            ))
            .stderr(predicate::str::contains("kitsu-context-rollback"));
    })
    .await
    .expect("join");
}

/// A user-selected output cap is already known when the provider returns
/// finish_reason=length. Preserve strict exit/disposition semantics, but turn
/// that value into an exact retry action in human and JSONL modes.
#[tokio::test]
#[serial]
async fn test_prompt_length_limit_names_current_cap_and_retry_value() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "partial but useful"},
                "finish_reason": "length"
            }],
            "usage": {"prompt_tokens": 8, "completion_tokens": 12, "total_tokens": 20}
        })))
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut human = base_cmd(&home_path);
        let output = human
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--no-stream",
                "--max-output-tokens",
                "12",
                "-p",
                "give a long answer",
            ])
            .output()
            .expect("run capped human prompt");
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stdout.contains("partial but useful"), "{stdout}");
        assert!(
            stdout.contains("[truncated: model output length limit reached]"),
            "{stdout}"
        );
        assert!(
            stderr.contains("assistant output above is provisional and was not committed"),
            "{stderr}"
        );
        assert!(stderr.contains("hit --max-output-tokens 12"), "{stderr}");
        assert!(
            stderr.contains("retry with --max-output-tokens 24 or higher"),
            "{stderr}"
        );
        assert!(!stderr.contains("DISCARDED"), "{stderr}");
        let timing = stderr
            .lines()
            .last()
            .and_then(|line| line.strip_prefix("kitsu-timing: "))
            .expect("failed model command ends with a timing receipt");
        let timing: serde_json::Value = serde_json::from_str(timing).unwrap();
        assert_eq!(timing["status"], "error");

        let mut jsonl = base_cmd(&home_path);
        let output = jsonl
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--max-output-tokens",
                "12",
                "-p",
                "give a long answer",
            ])
            .output()
            .expect("run capped JSONL prompt");
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stderr.is_empty(), "{:?}", output.stderr);
        let events = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let assistant = events
            .iter()
            .find(|event| event["type"] == "assistant")
            .expect("partial assistant event");
        assert!(assistant["text"]
            .as_str()
            .unwrap()
            .contains("partial but useful"));
        let disposition = events
            .iter()
            .find(|event| event["type"] == "assistant_disposition")
            .expect("discarded disposition");
        assert_eq!(disposition["status"], "discarded");
        assert!(disposition["reason"]
            .as_str()
            .unwrap()
            .contains("--max-output-tokens 24 or higher"));
        let error = events
            .iter()
            .find(|event| event["type"] == "error")
            .expect("strict error event");
        assert_eq!(error["exit_code"], 2);
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("hit --max-output-tokens 12"));
        assert_eq!(events.last().unwrap()["status"], "error");
        assert!(events.last().unwrap()["duration_ms"].is_u64());
    })
    .await
    .expect("capped prompt processes should join");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["max_tokens"], 12);
    }
    server.verify().await;
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
        assert_eq!(summary["tool_errors"], 1);
        assert_eq!(summary["policy_errors"], expected_policy_errors);
        assert_eq!(summary["recovered_tool_errors"], 0);
        assert_eq!(summary["recovered_policy_errors"], 0);
        let facts =
            jsonl_fixture::inspect_jsonl(&events, 1).expect("inspect strict tool-fault JSONL");
        assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::ToolFault);
        assert_eq!(facts.terminal_count, 1);
        assert_eq!(facts.provider_requests, 2);
        assert_eq!(facts.provider_attempts, 2);
        assert_eq!(facts.tool_started, 1);
        assert_eq!(facts.tool_finished, 1);
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
    let last_message = home.path().join("review-last-message.txt");

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
            "--output-last-message",
            &last_message.to_string_lossy(),
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
    let disclosures = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event["type"] == "review_disclosure")
        .collect::<Vec<_>>();
    assert_eq!(
        disclosures.len(),
        1,
        "top-level review must disclose its one bounded evidence payload exactly once: {events:#?}"
    );
    let (disclosure_index, disclosure) = disclosures[0];
    let turn_index = events
        .iter()
        .position(|event| event["type"] == "turn_started")
        .expect("review turn_started");
    let provider_index = events
        .iter()
        .position(|event| event["type"] == "provider_request")
        .expect("review provider_request");
    assert!(
        disclosure_index < turn_index && turn_index < provider_index,
        "disclosure must precede the turn that sends the evidence: {events:#?}"
    );
    assert_eq!(disclosure["source"], "top_level_review");
    assert_eq!(disclosure["target"], "working_tree");
    assert_eq!(disclosure["resolved_revision"], serde_json::Value::Null);
    assert_eq!(disclosure["merge_base"], serde_json::Value::Null);
    assert_eq!(disclosure["truncated"], false);
    assert_eq!(disclosure["model_disclosed"], true);
    assert_eq!(disclosure["model_read_root_granted"], false);
    assert!(disclosure["evidence_bytes"].as_u64().unwrap() > 0);
    let output_index = events
        .iter()
        .position(|event| event["type"] == "output_last_message")
        .expect("review final-output receipt");
    let turn_finished_index = events
        .iter()
        .position(|event| event["type"] == "turn_finished")
        .expect("review turn_finished");
    assert!(turn_finished_index < output_index && output_index < events.len() - 1);
    assert_eq!(
        events[output_index]["destination"],
        last_message.to_string_lossy().as_ref()
    );
    assert_eq!(
        events[output_index]["bytes"],
        "verified review complete".len()
    );
    assert_eq!(
        events[output_index]["sha256"],
        "3035ffbf3e7292f9a007c48056376c72f73ee5377f2d533916934223b2a3f7ce"
    );
    assert_eq!(events[output_index]["model_write_authority"], false);
    assert_eq!(
        fs::read(&last_message).unwrap(),
        b"verified review complete"
    );
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
        let disclosed_diff = prompt
            .split_once("```diff\n")
            .and_then(|(_, rest)| rest.split_once("\n```"))
            .map(|(diff, _)| diff)
            .expect("verified review diff block");
        assert_eq!(
            disclosure["evidence_bytes"].as_u64().unwrap(),
            disclosed_diff.len() as u64,
            "receipt byte count must describe the exact diff sent to the provider"
        );
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

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_no_artifacts_command_keeps_inline_receipt_without_creating_generation() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let tool_args = serde_json::json!({
        "argv": ["/bin/echo", "KITSU_INLINE_ONLY"],
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
                        "id": "inline_command",
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
                "message": {"role": "assistant", "content": "inline command checked"},
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
        "KITSU_INLINE_ONLY".to_string(),
    ])
    .unwrap();
    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(work.path())
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "--no-stream",
            "--no-artifacts",
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
    let run = &events[0];
    assert_eq!(run["type"], "run_started");
    assert_eq!(run["artifact_policy"]["mode"], "disabled");
    assert_eq!(run["artifact_policy"]["source"], "cli");
    assert_eq!(
        run["artifact_policy"]["new_automatic_artifact_writes"],
        false
    );
    assert_eq!(run["artifact_policy"]["command_output_persistence"], false);
    assert_eq!(
        run["artifact_policy"]["model_artifact_read_root_granted"],
        false
    );
    let finished = events
        .iter()
        .find(|event| event["type"] == "tool_finished")
        .expect("native tool_finished event");
    assert_eq!(finished["artifact_status"], "suppressed_by_policy");
    assert_eq!(finished["output_bytes"], 18);
    assert_eq!(finished["output_retained_bytes"], 18);
    assert_eq!(finished["output_truncated"], false);
    assert_eq!(
        finished["command"]["artifact_status"],
        "suppressed_by_policy"
    );
    assert!(finished["command"].get("artifact").is_none());
    assert!(finished["command"].get("artifact_error").is_none());
    assert_eq!(events.last().unwrap()["status"], "ok");
    assert!(
        !home.path().join(".kitsu/artifacts").exists(),
        "artifact-free stateless command run created an artifact namespace"
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let final_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert!(final_body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("KITSU_INLINE_ONLY"))));
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_no_artifacts_bounds_large_read_and_multibyte_command_output_inline() {
    use std::os::unix::fs::PermissionsExt as _;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn utf8_tail_boundary_stress(total_bytes: usize, glyph: &str, fill: u8) -> Vec<u8> {
        let tail_boundary = total_bytes - 32 * 1024;
        let glyph_start = tail_boundary - 1;
        let mut bytes = vec![fill; total_bytes];
        bytes[glyph_start..glyph_start + glyph.len()].copy_from_slice(glyph.as_bytes());
        let text = std::str::from_utf8(&bytes).expect("fixture remains valid UTF-8");
        assert!(
            !text.is_char_boundary(tail_boundary),
            "the retained tail must begin inside a multibyte scalar"
        );
        bytes
    }

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let input = work.path().join("large-read.txt");
    fs::write(
        &input,
        format!(
            "READ_HEAD{}READ_CENTER_MUST_BE_OMITTED{}READ_TAIL",
            "r".repeat(5_800),
            "s".repeat(5_800)
        ),
    )
    .unwrap();

    let stdout_bytes = utf8_tail_boundary_stress(106_062, "🦊", b'o');
    let stderr_bytes = utf8_tail_boundary_stress(105_051, "界", b'e');
    fs::write(work.path().join("stdout.bin"), &stdout_bytes).unwrap();
    fs::write(work.path().join("stderr.bin"), &stderr_bytes).unwrap();
    let emitter = work.path().join("emit-large-output.sh");
    fs::write(
        &emitter,
        "#!/bin/sh\n/bin/cat stdout.bin\n/bin/cat stderr.bin >&2\n",
    )
    .unwrap();
    fs::set_permissions(&emitter, fs::Permissions::from_mode(0o700)).unwrap();

    let read_args = serde_json::json!({"path": input}).to_string();
    let command_argv = vec![emitter.to_string_lossy().to_string()];
    let command_args = serde_json::json!({
        "argv": command_argv,
        "cwd": work.path(),
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"tool_call_id\":\"large_command\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "large outputs stayed bounded"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"tool_call_id\":\"large_read\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "large_command",
                        "type": "function",
                        "function": {"name": "command_exec", "arguments": command_args}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(5)
        .expect(1)
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
                        "id": "large_read",
                        "type": "function",
                        "function": {"name": "fs_read_file", "arguments": read_args}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let allow_command = serde_json::to_string(&command_argv).unwrap();
    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(work.path())
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "--no-stream",
            "--no-artifacts",
            "--context-window",
            "262144",
            "--max-rounds",
            "4",
            "chat",
            "--no-session",
            "--allow-path",
            &work.path().to_string_lossy(),
            "--enable-commands",
            "--command-root",
            &work.path().to_string_lossy(),
            "--allow-command",
            &allow_command,
            "read the fixture, run the approved emitter, and report completion",
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
    let read = events
        .iter()
        .find(|event| event["type"] == "tool_finished" && event["name"] == "fs_read_file")
        .expect("large read completion");
    assert_eq!(read["artifact_status"], "suppressed_by_policy");
    assert_eq!(read["output_truncated"], true);
    assert!(read["output_bytes"].as_u64().unwrap() > 11_600);
    assert!(read["output_retained_bytes"].as_u64().unwrap() <= 8 * 1024);

    let command = events
        .iter()
        .find(|event| event["type"] == "tool_finished" && event["name"] == "command_exec")
        .expect("large command completion");
    assert_eq!(command["artifact_status"], "suppressed_by_policy");
    assert_eq!(command["output_bytes"], 106_062 + 105_051);
    assert_eq!(command["output_retained_bytes"], 2 * 64 * 1024);
    assert_eq!(command["output_truncated"], true);
    assert_eq!(command["command"]["stdout_bytes"], 106_062);
    assert_eq!(command["command"]["stderr_bytes"], 105_051);
    assert_eq!(command["command"]["stdout_retained_bytes"], 64 * 1024);
    assert_eq!(command["command"]["stderr_retained_bytes"], 64 * 1024);
    assert_eq!(command["command"]["stdout_truncated"], true);
    assert_eq!(command["command"]["stderr_truncated"], true);
    assert_eq!(command["command"]["stdout_capture_complete"], true);
    assert_eq!(command["command"]["stderr_capture_complete"], true);
    assert_eq!(
        command["command"]["artifact_status"],
        "suppressed_by_policy"
    );
    assert!(command["command"].get("artifact").is_none());
    assert!(command["command"].get("artifact_error").is_none());
    assert_eq!(events.last().unwrap()["status"], "ok");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let read_request: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let read_content = read_request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["tool_call_id"] == "large_read")
        .and_then(|message| message["content"].as_str())
        .expect("bounded read result in second request");
    assert!(read_content.contains("bytes omitted by artifact-free policy"));
    assert!(!read_content.contains("READ_CENTER_MUST_BE_OMITTED"));
    let command_request: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
    let command_content = command_request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["tool_call_id"] == "large_command")
        .and_then(|message| message["content"].as_str())
        .expect("bounded command result in final request");
    let command_payload: serde_json::Value = serde_json::from_str(command_content).unwrap();
    assert_eq!(command_payload["stdout_bytes"], 106_062);
    assert_eq!(command_payload["stderr_bytes"], 105_051);
    assert!(command_payload["stdout"]
        .as_str()
        .unwrap()
        .contains("bytes omitted"));
    assert!(command_payload["stderr"]
        .as_str()
        .unwrap()
        .contains("bytes omitted"));
    assert!(
        !home.path().join(".kitsu/artifacts").exists(),
        "artifact-free command run created a global artifact namespace"
    );
    assert!(
        !home.path().join(".kitsu/sessions").exists(),
        "stateless artifact-free run created a session sidecar namespace"
    );
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_no_artifacts_bounds_large_tool_result_and_saves_session_without_sidecar() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let input = work.path().join("large.txt");
    fs::write(
        &input,
        format!(
            "HEAD{}CENTER_MUST_BE_OMITTED{}TAIL",
            "a".repeat(6_000),
            "b".repeat(6_000)
        ),
    )
    .unwrap();
    let read_args = serde_json::json!({"path": input.to_string_lossy()}).to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "large_read",
                        "type": "function",
                        "function": {"name": "fs_read_file", "arguments": read_args}
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
        .and(body_string_contains("\"tool_call_id\":\"large_read\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "large read checked"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "--no-stream",
            "--no-artifacts",
            "chat",
            "--session",
            "--allow-path",
            &work.path().to_string_lossy(),
        ])
        .write_stdin("inspect the large file\n")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let finished = events
        .iter()
        .find(|event| event["type"] == "tool_finished")
        .expect("tool completion");
    assert_eq!(finished["artifact_status"], "suppressed_by_policy");
    assert_eq!(finished["output_truncated"], true);
    assert!(finished["output_bytes"].as_u64().unwrap() > 12_000);
    assert!(finished["output_retained_bytes"].as_u64().unwrap() <= 8 * 1024);
    assert_eq!(events.last().unwrap()["status"], "ok");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    let tool_content = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .and_then(|message| message["content"].as_str())
        .expect("bounded tool result in second request");
    assert!(tool_content.contains("bytes omitted by artifact-free policy"));
    assert!(!tool_content.contains("CENTER_MUST_BE_OMITTED"));

    let sessions = home.path().join(".kitsu/sessions");
    let entries = fs::read_dir(&sessions)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(
        entries
            .iter()
            .filter(|path| path
                .extension()
                .is_some_and(|extension| extension == "json"))
            .count(),
        1,
        "durable session snapshot should still be saved"
    );
    assert!(
        entries.iter().all(|path| !path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .ends_with(".artifacts")),
        "artifact-free durable session created an artifact sidecar: {entries:?}"
    );
}

#[tokio::test]
#[serial]
async fn test_no_artifacts_resume_keeps_historical_pointer_but_grants_no_artifact_root() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIRST_PROMPT: &str = "HISTORICAL_ARTIFACT_POINTER_731";
    const RESUME_PROMPT: &str = "RESUME_WITHOUT_NEW_ARTIFACTS_947";

    fn snapshot_tree(root: &Path) -> Vec<(String, bool, Vec<u8>)> {
        fn visit(root: &Path, current: &Path, out: &mut Vec<(String, bool, Vec<u8>)>) {
            let mut entries = fs::read_dir(current)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            entries.sort();
            for entry in entries {
                let relative = entry
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                if entry.is_dir() {
                    out.push((relative, true, Vec::new()));
                    visit(root, &entry, out);
                } else {
                    out.push((relative, false, fs::read(&entry).unwrap()));
                }
            }
        }

        let mut snapshot = Vec::new();
        visit(root, root, &mut snapshot);
        snapshot
    }

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let input = work.path().join("historical-large.txt");
    fs::write(
        &input,
        format!("HISTORICAL_HEAD{}HISTORICAL_TAIL", "h".repeat(20_000)),
    )
    .unwrap();
    let read_args = serde_json::json!({"path": input}).to_string();

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(RESUME_PROMPT))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "resumed without new artifacts"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"tool_call_id\":\"historical_read\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "historical artifact saved"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(5)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains(FIRST_PROMPT))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "historical_read",
                        "type": "function",
                        "function": {"name": "fs_read_file", "arguments": read_args}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let first = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "--no-stream",
            "chat",
            "--session",
            "--allow-path",
            &work.path().to_string_lossy(),
            FIRST_PROMPT,
        ])
        .output()
        .unwrap();
    assert!(first.status.success(), "{first:?}");
    assert!(first.stderr.is_empty(), "{first:?}");
    let first_events = String::from_utf8(first.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let historical = first_events
        .iter()
        .find(|event| event["type"] == "tool_finished" && event["name"] == "fs_read_file")
        .expect("historical offload receipt");
    assert_eq!(historical["artifact_status"], "retained");
    assert!(historical["output"]
        .as_str()
        .unwrap()
        .contains("full result saved to"));
    let session_id = first_events.last().unwrap()["session_id"]
        .as_str()
        .expect("durable historical session")
        .to_string();
    let sidecar = home
        .path()
        .join(".kitsu/sessions")
        .join(format!("{session_id}.artifacts"));
    assert!(sidecar.is_dir(), "missing historical sidecar: {sidecar:?}");
    let before = snapshot_tree(&sidecar);
    assert!(
        before
            .iter()
            .any(|(_, is_dir, bytes)| !is_dir && !bytes.is_empty()),
        "historical sidecar should contain the offloaded result: {before:?}"
    );

    let resumed = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "--no-stream",
            "--no-artifacts",
            "chat",
            "--resume",
            &session_id,
            "--allow-path",
            &work.path().to_string_lossy(),
            RESUME_PROMPT,
        ])
        .output()
        .unwrap();
    assert!(resumed.status.success(), "{resumed:?}");
    assert!(resumed.stderr.is_empty(), "{resumed:?}");
    let resumed_events = String::from_utf8(resumed.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let run = &resumed_events[0];
    assert_eq!(run["type"], "run_started");
    assert_eq!(run["artifact_policy"]["mode"], "disabled");
    assert_eq!(run["artifact_policy"]["source"], "cli");
    assert_eq!(
        run["artifact_policy"]["new_automatic_artifact_writes"],
        false
    );
    assert_eq!(run["artifact_policy"]["existing_artifacts_unchanged"], true);
    assert_eq!(
        run["artifact_policy"]["model_artifact_read_root_granted"],
        false
    );
    assert_eq!(snapshot_tree(&sidecar), before);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let body: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    let system = messages
        .iter()
        .find(|message| message["role"] == "system")
        .and_then(|message| message["content"].as_str())
        .expect("refreshed system instruction");
    assert!(
        system.contains("No new saved full-output path is created"),
        "{system}"
    );
    assert!(
        system.contains("historical artifact pointers may remain in resumed context"),
        "{system}"
    );
    assert!(
        system.contains("no managed artifact read root is granted"),
        "{system}"
    );
    assert!(
        !system.contains("do not claim a saved full-output path exists"),
        "{system}"
    );
    assert!(
        messages.iter().any(|message| message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("full result saved to"))),
        "historical pointer was removed from resumed context: {messages:?}"
    );
    assert!(
        !home.path().join(".kitsu/artifacts").exists(),
        "artifact-free resume created a new global artifact namespace"
    );
    server.verify().await;
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
fn test_jsonl_unknown_script_commands_keep_actionable_host_hints() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home.path().join("jsonl-typos.txt");
    fs::write(&script_path, "/statsu\n/definitely-not-a-command-123456\n").unwrap();

    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "chat",
            "--script",
            &script_path.to_string_lossy(),
        ])
        .output()
        .expect("run JSONL typo script");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");

    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let errors = events
        .iter()
        .filter(|event| event["type"] == "error")
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 2, "{events:#?}");
    assert_eq!(errors[0]["line"], 1);
    assert_eq!(errors[0]["command"], "/statsu");
    assert_eq!(
        errors[0]["message"],
        "unknown command: /statsu - did you mean /status?"
    );
    assert_eq!(errors[1]["line"], 2);
    assert_eq!(errors[1]["command"], "/definitely-not-a-command-123456");
    assert_eq!(
        errors[1]["message"],
        "unknown command: /definitely-not-a-command-123456 (try /help)"
    );
    assert!(errors
        .iter()
        .all(|error| error["kind"] == "user" && error["exit_code"] == 1));
    let summary = events.last().expect("final summary");
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["failures"], 2);
    assert_eq!(summary["user_failures"], 2);
    assert_eq!(summary["successful_operations"], 0);

    // A summary-only consumer must not count `/quit` merely because its
    // command frame finished `ok`: only `/status` is a successful operation.
    let partial_script = home.path().join("jsonl-partial-typo.txt");
    fs::write(&partial_script, "/status\n/statsu\n/quit\n").unwrap();
    let partial = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "chat",
            "--script",
            &partial_script.to_string_lossy(),
            "--allow-turn-errors",
            "--no-session",
        ])
        .output()
        .expect("run accepted-partial JSONL typo script");
    assert!(partial.status.success(), "{partial:?}");
    assert!(partial.stderr.is_empty(), "{partial:?}");
    let partial_events = String::from_utf8(partial.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let partial_summary = partial_events.last().expect("partial final summary");
    assert_eq!(partial_summary["type"], "final_summary");
    assert_eq!(partial_summary["status"], "partial");
    assert_eq!(partial_summary["failures"], 1);
    assert_eq!(partial_summary["user_failures"], 1);
    assert_eq!(partial_summary["successful_operations"], 1);
}

#[test]
fn test_missing_checkpoint_keeps_actionable_host_hint_in_jsonl_and_human_scripts() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home.path().join("missing-checkpoint.txt");
    fs::write(&script_path, "/checkpoints\n/checkpoint 54007\n/quit\n").unwrap();
    let message =
        "checkpoint #54007 is not available; run /checkpoints to list recoverable checkpoints";

    let jsonl = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "chat",
            "--no-session",
            "--no-tools",
            "--script",
            &script_path.to_string_lossy(),
        ])
        .output()
        .expect("run missing-checkpoint JSONL script");
    assert_eq!(jsonl.status.code(), Some(1), "{jsonl:?}");
    assert!(jsonl.stderr.is_empty(), "{jsonl:?}");
    let events = String::from_utf8(jsonl.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "run_started",
            "command_started",
            "command_result",
            "command_finished",
            "command_started",
            "error",
            "command_finished",
            "command_started",
            "command_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[2]["result"]["kind"], "checkpoints");
    assert_eq!(events[2]["result"]["count"], 0);
    assert_eq!(events[5]["line"], 2);
    assert_eq!(events[5]["command"], "/checkpoint 54007");
    assert_eq!(events[5]["kind"], "user");
    assert_eq!(events[5]["message"], message);
    assert_eq!(events[5]["exit_code"], 1);
    assert_eq!(events[6]["status"], "error");
    assert_eq!(events[9]["status"], "error");
    assert_eq!(events[9]["failures"], 1);
    assert_eq!(events[9]["user_failures"], 1);
    assert_eq!(events[9]["successful_operations"], 1);
    assert!(!events
        .iter()
        .any(|event| event["type"] == "provider_request"));

    let human = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "chat",
            "--no-session",
            "--no-tools",
            "--script",
            &script_path.to_string_lossy(),
        ])
        .output()
        .expect("run missing-checkpoint human script");
    assert_eq!(human.status.code(), Some(1), "{human:?}");
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert_eq!(stderr.matches(message).count(), 1, "{stderr}");
    assert!(
        stderr.contains(&format!("{}:2: {message}", script_path.display())),
        "{stderr}"
    );
    assert!(!stderr.contains("command failed: /checkpoint 54007"));
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
fn test_scripted_queue_command_reports_queue_specific_recovery() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script_path = home.path().join("scripted-queue.txt");
    fs::write(&script_path, "/status\n/queue list\n/quit\n").unwrap();
    let message = "queue commands require interactive chat; put prompts on later script lines";

    let jsonl = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "chat",
            "--no-session",
            "--no-tools",
            "--allow-turn-errors",
            "--script",
            &script_path.to_string_lossy(),
        ])
        .output()
        .expect("run scripted queue JSONL case");
    assert!(jsonl.status.success(), "{jsonl:?}");
    assert!(jsonl.stderr.is_empty(), "{jsonl:?}");
    let events = String::from_utf8(jsonl.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "run_started",
            "command_started",
            "command_result",
            "command_finished",
            "command_started",
            "error",
            "command_finished",
            "command_started",
            "command_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[5]["line"], 2);
    assert_eq!(events[5]["command"], "/queue list");
    assert_eq!(events[5]["kind"], "user");
    assert_eq!(events[5]["message"], message);
    assert_eq!(events[5]["exit_code"], 1);
    assert_eq!(events[6]["status"], "error");
    let summary = events.last().expect("final summary");
    assert_eq!(summary["status"], "partial");
    assert_eq!(summary["failures"], 1);
    assert_eq!(summary["user_failures"], 1);
    assert_eq!(summary["successful_operations"], 1);
    assert_eq!(summary["session_snapshot_state"], "disabled");
    assert!(summary["session_id"].is_null());
    assert!(!events
        .iter()
        .any(|event| event["type"] == "provider_request"));

    let human = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "chat",
            "--no-session",
            "--no-tools",
            "--allow-turn-errors",
            "--script",
            &script_path.to_string_lossy(),
        ])
        .output()
        .expect("run scripted queue human case");
    assert!(human.status.success(), "{human:?}");
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert_eq!(stderr.matches(message).count(), 1, "{stderr}");
    assert!(
        stderr.contains(&format!("{}:2: {message}", script_path.display())),
        "{stderr}"
    );
    assert!(!stderr.contains("session commands require interactive chat"));
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
            .stderr(predicate::str::contains("* alpha-model (current)"))
            .stderr(predicate::str::contains("beta-model"))
            .stderr(predicate::str::contains("2 model(s)"))
            .stderr(predicate::str::contains("switch with: /model <id>"));
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
    let kinds = events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "run_started",
            "command_started",
            "provider_request",
            "provider_attempt",
            "error",
            "command_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[2]["line"], 1);
    assert_eq!(events[2]["command"], "/models");
    assert_eq!(events[2]["purpose"], "model_catalog");
    assert_eq!(events[2]["message_count"], 0);
    assert_eq!(events[2]["tools_offered"], 0);
    assert_eq!(events[3]["line"], 1);
    assert_eq!(events[3]["command"], "/models");
    assert_eq!(events[3]["purpose"], "model_catalog");
    assert_eq!(events[3]["category"], "unavailable");
    assert_eq!(events[3]["status"], 503);
    assert_eq!(events[3]["outcome"], "rejected");
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
            "provider_request",
            "provider_attempt",
            "error",
            "command_finished",
            "final_summary",
        ]
    );
    assert_eq!(events[2]["line"], 1);
    assert_eq!(events[2]["command"], "/models");
    assert_eq!(events[2]["purpose"], "model_catalog");
    assert_eq!(events[3]["line"], 1);
    assert_eq!(events[3]["command"], "/models");
    assert_eq!(events[3]["purpose"], "model_catalog");
    assert_eq!(events[3]["category"], "cancelled");
    assert_eq!(events[3]["status"], serde_json::Value::Null);
    assert_eq!(events[3]["outcome"], "cancelled");
    assert_eq!(events[5]["status"], "cancelled");
    assert_eq!(events[6]["status"], "cancelled");
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
        "cwd": sessions_dir.to_string_lossy(),
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
    let allow_command = serde_json::json!({
        "argv": [
            "/bin/chmod",
            "0500",
            sessions_dir.to_string_lossy().into_owned(),
        ],
        "cwd": sessions_dir.to_string_lossy(),
    })
    .to_string();
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
            &sessions_dir.to_string_lossy(),
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
    assert!(events[summary_index]["session_id"].is_null());
    assert_eq!(
        events[summary_index]["session_snapshot_state"],
        "pending_until_first_success"
    );
    let mut sessions = base_cmd(home.path());
    let listed = sessions
        .args(["sessions", "--json"])
        .output()
        .expect("list sessions after pre-install save failure");
    assert!(listed.status.success(), "{listed:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&listed.stdout).unwrap(),
        serde_json::json!([])
    );
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

    let ((created, created_facts), (resumed, resumed_facts)) =
        tokio::task::spawn_blocking(move || {
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
                let exit_code = output.status.code().expect("process exit code");
                let events = String::from_utf8(output.stdout)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .collect::<Vec<_>>();
                let facts = jsonl_fixture::inspect_jsonl(&events, exit_code)
                    .expect("inspect same-session JSONL run");
                let started = events
                    .into_iter()
                    .find(|event| event["type"] == "run_started")
                    .expect("run_started");
                (started, facts)
            };

            let (created, created_facts) =
                run(&["--session", "--model", "saved-model"], &create_script);
            let id = created["session_id"]
                .as_str()
                .expect("new durable session id")
                .to_string();
            let (resumed, resumed_facts) = run(&["--resume", &id], &resume_script);
            ((created, created_facts), (resumed, resumed_facts))
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
    assert_ne!(created_facts.run_id, resumed_facts.run_id);
    assert_eq!(
        created_facts.class,
        jsonl_fixture::JsonlOutcomeClass::Succeeded
    );
    assert_eq!(created_facts.provider_requests, 1);
    assert_eq!(
        resumed_facts.class,
        jsonl_fixture::JsonlOutcomeClass::Succeeded
    );
    assert_eq!(resumed_facts.provider_requests, 0);
    assert_eq!(created_facts.terminal_count, 1);
    assert_eq!(resumed_facts.terminal_count, 1);
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
    let facts = jsonl_fixture::inspect_jsonl(&events, 0)
        .expect("inspect successful background join before terminal");
    assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::Succeeded);
    assert_eq!(facts.terminal_count, 1);
    assert_eq!(facts.provider_requests, 2);
    assert_eq!(facts.provider_attempts, 2);
    assert_eq!(facts.tool_started, facts.tool_finished);
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

#[tokio::test]
#[serial]
async fn test_piped_deadline_preserves_completed_tool_round_as_resumable_session() {
    use std::time::Duration;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());

    // The model's first (and only completed) round reads this file; its marker
    // proves the tool round completed and was persisted rather than discarded.
    let evidence = home.path().join("evidence.txt");
    fs::write(&evidence, "TOOL_ROUND_EVIDENCE_MARKER").unwrap();
    let read_args = serde_json::json!({ "path": evidence.to_string_lossy() }).to_string();

    // Round 2 (the request that carries the tool result back) stalls far past
    // the turn deadline, forcing a mid-turn timeout after one complete round.
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "arrived after the deadline"},
                        "finish_reason": "stop"
                    }]
                })),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    // Round 1: the model asks to read the evidence file (a complete tool round).
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "read_evidence",
                        "type": "function",
                        "function": {"name": "fs_read_file", "arguments": read_args}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .mount(&server)
        .await;

    // A positional prompt over a non-tty stdin drives the piped/session path
    // (the automation surface the fix targets), not scripted or interactive.
    // JSONL is intentional here: the regression must prove that a synthetic
    // deadline marker is attributed to the request round that timed out.
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "--no-stream",
                "--timeout-secs",
                "3",
                "chat",
                "--session",
                "--allow-path",
                &home_path.to_string_lossy(),
                "read the evidence file",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    // The turn times out mid-second-round and exits nonzero...
    assert!(
        !output.status.success(),
        "expected a deadline timeout: {output:?}"
    );

    // ...but the completed tool round is preserved to a resumable session
    // instead of being discarded. The persisted transcript must carry both the
    // tool result and the honest deadline marker.
    let sessions_dir = home.path().join(".kitsu").join("sessions");
    let persisted = fs::read_dir(&sessions_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().map(|ext| ext == "json").unwrap_or(false))
        .filter_map(|path| fs::read_to_string(path).ok())
        .any(|body| {
            body.contains("TOOL_ROUND_EVIDENCE_MARKER") && body.contains("deadline expired")
        });
    assert!(
        persisted,
        "a piped turn that times out after a completed tool round must persist it as a \
         resumable session (none found under {sessions_dir:?})"
    );

    let events = String::from_utf8(output.stdout)
        .expect("JSONL stdout must be UTF-8")
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|error| panic!("invalid JSONL line ({error}): {line}"))
        })
        .collect::<Vec<_>>();
    let marker_delta = events
        .iter()
        .find(|event| event["type"] == "assistant_delta")
        .expect("deadline marker delta");
    assert_eq!(
        marker_delta["round"], 2,
        "marker belongs to timed-out round"
    );
    assert_eq!(
        marker_delta["text"],
        "\n[stopped: deadline expired - completed tool results were preserved; no final answer was produced]"
    );
    let marker_assistant = events
        .iter()
        .find(|event| event["type"] == "assistant")
        .expect("synthetic resumable assistant marker");
    assert_eq!(marker_assistant["text"], marker_delta["text"]);
    let disposition = events
        .iter()
        .find(|event| event["type"] == "assistant_disposition")
        .expect("partial disposition");
    assert_eq!(disposition["status"], "committed_partial");
    assert!(disposition["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("no final answer was produced")));
    let summary = events.last().expect("final summary");
    assert_eq!(summary["type"], "final_summary");
    assert_eq!(summary["status"], "error");
    assert_eq!(summary["session_snapshot_state"], "resumable");
    let session_id = summary["session_id"]
        .as_str()
        .expect("resumable session id")
        .to_string();

    let mut show = base_cmd(home.path());
    let shown = show
        .args(["sessions", "--json", "show", &session_id])
        .output()
        .expect("show preserved session");
    assert!(shown.status.success(), "{shown:?}");
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let messages = shown["messages"].as_array().expect("saved transcript");
    let tool_ids = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter_map(|message| message["tool_call_id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(
        messages.iter().all(|message| {
            message["role"] != "assistant"
                || message["tool_calls"].as_array().map_or(true, |calls| {
                    calls.iter().all(|call| {
                        call["call_id"]
                            .as_str()
                            .is_some_and(|call_id| tool_ids.contains(call_id))
                    })
                })
        }),
        "preserved transcript contains a dangling tool call: {messages:?}"
    );
    assert_eq!(messages.last().unwrap()["content"], marker_delta["text"]);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "one request per completed/timed-out round"
    );
}
