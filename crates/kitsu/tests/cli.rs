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
    drive: F,
) -> (std::process::ExitStatus, String)
where
    F: FnMut(&[u8], &mut fs::File),
{
    run_plain_interactive_chat_pty_with_env(home, cwd, extra_chat_args, &[], drive)
}

/// [`run_plain_interactive_chat_pty`] with extra environment for the child
/// only. T2-S7 needs it to point the carrier's agent state file at a private
/// `XDG_RUNTIME_DIR`; the parent's environment is never touched, which is the
/// same rule every other state-file test follows.
#[cfg(unix)]
fn run_plain_interactive_chat_pty_with_env<F>(
    home: &Path,
    cwd: &Path,
    extra_chat_args: &[&str],
    extra_env: &[(&str, &Path)],
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
                std::ptr::null_mut(),
                std::ptr::null_mut(),
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
    for (name, value) in extra_env {
        command.env(name, value);
    }
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
    let run_started = events
        .iter()
        .find(|event| event["type"] == "run_started")
        .expect("run_started route receipt");
    let provider_attempt = events
        .iter()
        .find(|event| event["type"] == "provider_attempt")
        .expect("provider attempt");
    assert_eq!(
        run_started["provider_route"]["route"]["adapter"],
        "openai_compatible"
    );
    assert_eq!(
        provider_attempt["route_generation"],
        run_started["provider_route"]["route"]["generation"]
    );
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
    assert_eq!(model_events[0]["provider_route"]["receipt_revision"], 1);
    assert_eq!(
        model_events[0]["provider_route"]["route"]["adapter"],
        "openai_compatible"
    );
    assert_eq!(
        model_events[0]["provider_route"]["model"],
        serde_json::Value::Null
    );
    assert_eq!(
        model_events[0]["provider_route"]["route"]["capabilities"]["supported"],
        serde_json::json!(["model_catalog"])
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
    assert_eq!(
        model_events[2]["route_generation"],
        model_events[0]["provider_route"]["route"]["generation"]
    );
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
    assert_eq!(
        report["provider_route"]["status"],
        "not_constructed_offline"
    );
    assert_eq!(report["provider_route"]["receipt"], serde_json::Value::Null);
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
        // The home accessor falls back to USERPROFILE (issue #26); a Windows
        // dev box exporting it must not turn this into a passing login.
        .env_remove("USERPROFILE")
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
    assert!(combined.contains("requires an absolute home directory"));
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
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
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
    fs::write(&script, "/status\n/context sources\n/sessions\n/quit\n").unwrap();

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

#[tokio::test]
#[serial]
async fn test_usage_aliases_share_one_offline_truthful_snapshot_without_jsonl_payloads() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let home = TempDir::new().expect("usage-session home");
    write_config(home.path(), &server.uri());

    let mut human_decks = Vec::new();
    for (name, command) in [
        ("usage-session", "/usage session"),
        ("usage-bare", "/usage"),
        ("cost-compat", "/cost"),
    ] {
        let script = home.path().join(format!("{name}.script"));
        fs::write(&script, format!("{command}\n/quit\n")).unwrap();
        let human = base_cmd(home.path())
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--plain",
                "chat",
                "--no-session",
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("run offline current-session usage alias");
        assert!(human.status.success(), "{human:?}");
        assert!(human.stdout.is_empty(), "{human:?}");
        let human_stderr = String::from_utf8(human.stderr).unwrap();
        assert!(human_stderr.contains("[usage]"), "{human_stderr}");
        assert!(
            human_stderr.contains("usage.responses reported: 0"),
            "{human_stderr}"
        );
        assert!(
            human_stderr.contains("usage.prompt tokens: unavailable"),
            "{human_stderr}"
        );
        assert!(!human_stderr.contains('\u{1b}'), "{human_stderr}");
        let lower = human_stderr.to_ascii_lowercase();
        for prohibited in [
            "account:",
            "credits",
            "rate limit",
            "price",
            "currency",
            "usd",
            "$",
        ] {
            assert!(!lower.contains(prohibited), "{human_stderr}");
        }
        let deck_start = human_stderr
            .find("current session\n[usage]\n")
            .expect("plain typed usage deck start");
        let deck = &human_stderr[deck_start..];
        let deck_end = deck.find("\ncommand>").unwrap_or(deck.len());
        human_decks.push(deck[..deck_end].to_string());

        let jsonl = base_cmd(home.path())
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--jsonl",
                "chat",
                "--no-session",
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("run JSONL usage alias without mounting a payload");
        assert!(jsonl.status.success(), "{jsonl:?}");
        assert!(jsonl.stderr.is_empty(), "{jsonl:?}");
        let events: Vec<serde_json::Value> = String::from_utf8(jsonl.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
            .collect();
        assert!(events
            .iter()
            .any(|event| { event["type"] == "command_started" && event["command"] == command }));
        assert!(events.iter().any(|event| {
            event["type"] == "command_finished"
                && event["command"] == command
                && event["status"] == "ok"
        }));
        assert!(!events
            .iter()
            .any(|event| { event["type"] == "command_result" && event["command"] == command }));
    }
    assert_eq!(human_decks[0], human_decks[1]);
    assert_eq!(human_decks[0], human_decks[2]);

    for (name, contents, syntax) in [
        (
            "usage-unknown.script",
            "/usage current\n/quit\n",
            "usage: /usage [session]",
        ),
        ("cost-args.script", "/cost session\n/quit\n", "usage: /cost"),
    ] {
        let script = home.path().join(name);
        fs::write(&script, contents).unwrap();
        let rejected = base_cmd(home.path())
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--plain",
                "chat",
                "--no-session",
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("reject inexact usage command");
        assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
        assert!(String::from_utf8(rejected.stderr).unwrap().contains(syntax));
    }
}

#[tokio::test]
#[serial]
async fn test_context_window_is_offline_exact_accessible_and_leaves_jsonl_unmounted() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let home = TempDir::new().expect("context-window home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("context-window.script");
    fs::write(&script, "/context window\n/quit\n").unwrap();

    for (mode, args, env) in [
        ("plain", vec!["--plain"], None),
        ("no-color", vec![], Some(("NO_COLOR", "1"))),
        ("screen-reader", vec!["--screen-reader"], None),
        ("redirected", vec![], None),
    ] {
        let mut command = base_cmd(home.path());
        command.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        if let Some((name, value)) = env {
            command.env(name, value);
        } else {
            command.env_remove("NO_COLOR");
        }
        command.args(args).args([
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ]);
        let output = command.output().expect("run context-window mode");
        assert!(output.status.success(), "{mode}: {output:?}");
        assert!(output.stdout.is_empty(), "{mode}: {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("[context]"), "{mode}: {stderr}");
        assert!(stderr.contains("context.occupancy:"), "{mode}: {stderr}");
        assert!(
            stderr.contains("context.window tokens:"),
            "{mode}: {stderr}"
        );
        assert!(
            stderr.contains("context.available tokens:"),
            "{mode}: {stderr}"
        );
        assert!(!stderr.contains("[status]"), "{mode}: {stderr}");
        assert!(!stderr.contains("[usage]"), "{mode}: {stderr}");
        assert!(!stderr.contains("status.adapter:"), "{mode}: {stderr}");
        assert!(!stderr.contains("usage.prompt tokens:"), "{mode}: {stderr}");
        assert!(!stderr.contains('\u{1b}'), "{mode}: {stderr}");
    }

    let jsonl = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("run JSONL context-window command");
    assert!(jsonl.status.success(), "{jsonl:?}");
    assert!(jsonl.stderr.is_empty(), "{jsonl:?}");
    let events: Vec<serde_json::Value> = String::from_utf8(jsonl.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "command_finished"
            && event["command"] == "/context window"
            && event["status"] == "ok"
    }));
    assert!(!events.iter().any(|event| {
        event["type"] == "command_result" && event["command"] == "/context window"
    }));

    for (name, contents) in [
        ("context-windows.script", "/context windows\n/quit\n"),
        (
            "context-window-extra.script",
            "/context window extra\n/quit\n",
        ),
    ] {
        let inexact = home.path().join(name);
        fs::write(&inexact, contents).unwrap();
        let rejected = base_cmd(home.path())
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--plain",
                "chat",
                "--no-session",
                "--script",
                &inexact.to_string_lossy(),
            ])
            .output()
            .expect("reject inexact context-window form");
        assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
        assert!(String::from_utf8(rejected.stderr)
            .unwrap()
            .contains("usage: /context [window|sources|refresh]"));
    }

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "/context window sent a provider request"
    );
}

#[tokio::test]
#[serial]
async fn test_context_sources_is_offline_accessible_and_lifecycle_only_in_jsonl() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let home = TempDir::new().expect("context-sources home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("context-sources.script");
    fs::write(&script, "/context sources\n/quit\n").unwrap();

    for (mode, args, env) in [
        ("plain", vec!["--plain"], None),
        ("no-color", vec![], Some(("NO_COLOR", "1"))),
        ("screen-reader", vec!["--screen-reader"], None),
        ("redirected", vec![], None),
    ] {
        let mut command = base_cmd(home.path());
        command.env("ANTHROPIC_AUTH_TOKEN", TOKEN);
        if let Some((name, value)) = env {
            command.env(name, value);
        } else {
            command.env_remove("NO_COLOR");
        }
        command.args(args).args([
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ]);
        let output = command.output().expect("run context-sources mode");
        assert!(output.status.success(), "{mode}: {output:?}");
        assert!(output.stdout.is_empty(), "{mode}: {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        for required in [
            "[sources]",
            "source.core instructions:",
            "source.project context:",
            "source.user messages:",
            "source.attachments: unavailable (included in user messages)",
            "source.on-demand skill bodies: unavailable (included in user messages)",
            "source.memory: unavailable (no dedicated Kitsu memory bucket)",
            "context.output reserve:",
            "context.tokens saved:",
        ] {
            assert!(
                stderr.contains(required),
                "{mode}: missing {required}: {stderr}"
            );
        }
        assert!(!stderr.contains("source total"), "{mode}: {stderr}");
        assert!(!stderr.contains('\u{1b}'), "{mode}: {stderr}");
        for forbidden in ["credits", "rate limit", "price", "account:", "@"] {
            assert!(
                !stderr.to_ascii_lowercase().contains(forbidden),
                "{mode}: invented or leaked {forbidden}: {stderr}"
            );
        }
    }

    let jsonl = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--jsonl",
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("run JSONL context-sources command");
    assert!(jsonl.status.success(), "{jsonl:?}");
    assert!(jsonl.stderr.is_empty(), "{jsonl:?}");
    let events: Vec<serde_json::Value> = String::from_utf8(jsonl.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "command_started" && event["command"] == "/context sources"
    }));
    assert!(events.iter().any(|event| {
        event["type"] == "command_finished"
            && event["command"] == "/context sources"
            && event["status"] == "ok"
    }));
    assert!(!events.iter().any(|event| {
        event["type"] == "command_result" && event["command"] == "/context sources"
    }));

    for (name, contents) in [
        ("context-source.script", "/context source\n/quit\n"),
        (
            "context-sources-extra.script",
            "/context sources extra\n/quit\n",
        ),
    ] {
        let inexact = home.path().join(name);
        fs::write(&inexact, contents).unwrap();
        let rejected = base_cmd(home.path())
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--plain",
                "chat",
                "--no-session",
                "--script",
                &inexact.to_string_lossy(),
            ])
            .output()
            .expect("reject inexact context-sources form");
        assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
        assert!(String::from_utf8(rejected.stderr)
            .unwrap()
            .contains("usage: /context [window|sources|refresh]"));
    }

    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "/context sources sent a provider request"
    );
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
                std::ptr::null_mut(),
                std::ptr::null_mut(),
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
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
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
        // Card H2-title. A non-styled surface still emits no OSC at all;
        // a styled TTY is exactly where requirement H2 demands the terminal
        // title, so the assertion pins *which* OSC appears rather than
        // flipping to a blanket allowance (**F41**): every OSC introducer on
        // a styled surface must open the state title.
        let osc_count = output
            .windows(b"\x1b]".len())
            .filter(|window| *window == b"\x1b]")
            .count();
        if styled {
            let title_intro = "\x1b]2;kitsu \u{b7} ".as_bytes();
            let title_count = output
                .windows(title_intro.len())
                .filter(|window| *window == title_intro)
                .count();
            assert_eq!(
                osc_count, title_count,
                "{name}: the only OSC a styled surface may emit is the H2 \
                 terminal title: {output:?}"
            );
        } else {
            assert_eq!(osc_count, 0, "{name} emitted an OSC sequence: {output:?}");
        }
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
    let mut size = libc::winsize {
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
                std::ptr::null_mut(),
                &mut size,
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
async fn explicit_anthropic_messages_adapter_drives_the_live_native_wire() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        concat!(
            "event: message_start\n",
            r#"data: {"type":"message_start","message":{"id":"msg_cli_native","type":"message","role":"assistant","content":[],"model":"native-model","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":8,"output_tokens":0}}}"#
        ),
        concat!(
            "event: content_block_start\n",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#
        ),
        concat!(
            "event: content_block_delta\n",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello from native Messages"}}"#
        ),
        concat!(
            "event: content_block_stop\n",
            r#"data: {"type":"content_block_stop","index":0}"#
        ),
        concat!(
            "event: message_delta\n",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":4}}"#
        ),
        concat!(
            "event: message_stop\n",
            r#"data: {"type":"message_stop"}"#
        ),
    ]
    .join("\n\n");
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("{sse}\n\n")),
        )
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "native"

[gateways.native]
flavor = "anthropic"
adapter = "anthropic-messages"
base_url = "{}"
chat_path = "/v1/messages"
models_path = ""
model = "native-model"
context_window = 8192
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.args([
            "--max-output-tokens",
            "64",
            "chat",
            "--no-tools",
            "hello native",
        ]);
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("hello from native Messages"));

        let mut doctor = base_cmd(&home_path);
        let output = doctor
            .args(["doctor", "--inference", "--json"])
            .output()
            .expect("run native inference doctor");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["online"]["status"], "reachable");
        assert_eq!(report["online"]["effective_model"], "native-model");
        assert_eq!(report["provider_route"]["status"], "bound");
        assert_eq!(
            report["provider_route"]["receipt"]["route"]["adapter"],
            "anthropic_messages"
        );
        assert_eq!(
            report["provider_route"]["receipt"]["route"]["wire_revision"],
            "anthropic_messages_2023_06_01"
        );
    })
    .await
    .expect("native CLI process should join");

    let requests = server.received_requests().await.expect("record request");
    assert_eq!(requests.len(), 2);
    let chat_body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(chat_body["model"], "native-model");
    assert_eq!(chat_body["max_tokens"], 64);
    assert_eq!(chat_body["stream"], true);
    assert_eq!(
        chat_body["messages"],
        serde_json::json!([{
            "role": "user",
            "content": "hello native"
        }])
    );
    assert!(chat_body.get("system").is_some());
    assert!(chat_body.get("tools").is_none());
    assert!(chat_body.get("stream_options").is_none());

    let doctor_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(doctor_body["model"], "native-model");
    assert_eq!(doctor_body["max_tokens"], 1);
    assert_eq!(doctor_body["stream"], true);
    assert_eq!(
        doctor_body["messages"],
        serde_json::json!([{"role": "user", "content": "Reply OK."}])
    );
    assert!(doctor_body.get("tools").is_none());
}

#[derive(Clone, Copy)]
struct ProviderProductCase {
    adapter_config: &'static str,
    adapter_receipt: &'static str,
    wire_receipt: &'static str,
    chat_path: &'static str,
    capabilities: &'static [&'static str],
}

const PROVIDER_PRODUCT_CASES: [ProviderProductCase; 2] = [
    ProviderProductCase {
        adapter_config: "compatible-chat",
        adapter_receipt: "openai_compatible",
        wire_receipt: "openai_chat_completions_v1",
        chat_path: "/product/chat/completions",
        capabilities: &["streaming", "tools", "structured_output", "usage"],
    },
    ProviderProductCase {
        adapter_config: "anthropic-messages",
        adapter_receipt: "anthropic_messages",
        wire_receipt: "anthropic_messages_2023_06_01",
        chat_path: "/product/messages",
        capabilities: &["streaming", "tools", "usage"],
    },
];

fn write_provider_product_config(
    home: &Path,
    base_url: &str,
    case: ProviderProductCase,
    chat_path: &str,
) {
    let kitsu_dir = home.join(".kitsu");
    fs::create_dir_all(&kitsu_dir).expect("create product-conformance config directory");
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "product"

[gateways.product]
flavor = "openai"
adapter = "{}"
base_url = "{}"
chat_path = "{}"
models_path = ""
model = "product-model"
context_window = 8192
auth = {{ method = "none" }}
"#,
            case.adapter_config, base_url, chat_path
        ),
    )
    .expect("write product-conformance config");
}

fn provider_product_sse(case: ProviderProductCase) -> String {
    if case.adapter_receipt == "openai_compatible" {
        return [
            r#"data: {"choices":[{"index":0,"delta":{"content":"product answer"},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
            "",
        ]
        .join("\n\n");
    }
    [
        concat!(
            "event: message_start\n",
            r#"data: {"type":"message_start","message":{"id":"msg_product","type":"message","role":"assistant","content":[],"model":"product-model","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":0}}}"#
        ),
        concat!(
            "event: content_block_start\n",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#
        ),
        concat!(
            "event: content_block_delta\n",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"product answer"}}"#
        ),
        concat!(
            "event: content_block_stop\n",
            r#"data: {"type":"content_block_stop","index":0}"#
        ),
        concat!(
            "event: message_delta\n",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}"#
        ),
        concat!(
            "event: message_stop\n",
            r#"data: {"type":"message_stop"}"#
        ),
    ]
    .join("\n\n")
        + "\n\n"
}

fn parse_product_jsonl(output: &std::process::Output) -> Vec<serde_json::Value> {
    String::from_utf8(output.stdout.clone())
        .expect("product JSONL is UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("product JSONL line"))
        .collect()
}

fn assert_product_receipt(
    receipt: &serde_json::Value,
    case: ProviderProductCase,
    expected_model: Option<&str>,
) {
    assert_eq!(receipt["receipt_revision"], 1);
    assert_eq!(receipt["route"]["contract_revision"], 1);
    assert_eq!(receipt["route"]["adapter"], case.adapter_receipt);
    assert_eq!(receipt["route"]["wire_revision"], case.wire_receipt);
    assert_eq!(receipt["route"]["generation"], 1);
    assert_eq!(
        receipt["route"]["capabilities"]["supported"],
        serde_json::json!(case.capabilities)
    );
    assert_eq!(receipt["model"], serde_json::json!(expected_model));
    assert_eq!(receipt["auth_source"], "none");
    assert_eq!(receipt["policy"]["retry_generation"], "bound_generation");
    assert_eq!(
        receipt["policy"]["semantic_replay_generation"],
        "current_generation"
    );
    assert_eq!(receipt["policy"]["fallback"], "disabled");
}

#[tokio::test]
#[serial]
async fn both_compiled_adapters_pass_product_route_receipt_conformance() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    for case in PROVIDER_PRODUCT_CASES {
        let server = MockServer::start().await;
        let sse = provider_product_sse(case);
        if case.adapter_receipt == "openai_compatible" {
            Mock::given(method("POST"))
                .and(path(case.chat_path))
                .and(body_string_contains("\"stream\":true"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(sse),
                )
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path(case.chat_path))
                .and(body_string_contains("\"stream\":false"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "OK"},
                        "finish_reason": "stop"
                    }]
                })))
                .expect(1)
                .mount(&server)
                .await;
        } else {
            Mock::given(method("POST"))
                .and(path(case.chat_path))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(sse),
                )
                .expect(2)
                .mount(&server)
                .await;
        }
        let fault_path = format!("{}/fault", case.chat_path);
        Mock::given(method("POST"))
            .and(path(fault_path.as_str()))
            .respond_with(ResponseTemplate::new(401).set_body_string("provider-secret-body"))
            .expect(1)
            .mount(&server)
            .await;

        let home = TempDir::new().expect("product conformance home");
        write_provider_product_config(home.path(), &server.uri(), case, case.chat_path);
        let script = home.path().join("product.script");
        fs::write(&script, "exercise the product provider seam\n").unwrap();
        let success = base_cmd(home.path())
            .args([
                "--jsonl",
                "chat",
                "--no-session",
                "--no-tools",
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("run product conformance success");
        assert!(success.status.success(), "{success:?}");
        assert!(success.stderr.is_empty(), "{success:?}");
        let success_events = parse_product_jsonl(&success);
        let run_started = success_events
            .iter()
            .find(|event| event["type"] == "run_started")
            .expect("success run_started");
        let run_receipt = &run_started["provider_route"];
        assert_product_receipt(run_receipt, case, Some("product-model"));
        let attempts = success_events
            .iter()
            .filter(|event| event["type"] == "provider_attempt")
            .collect::<Vec<_>>();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["category"], "success");
        assert_eq!(attempts[0]["terminal"], true);
        assert_eq!(
            attempts[0]["route_generation"],
            run_receipt["route"]["generation"]
        );
        assert!(!run_receipt.to_string().contains(&server.uri()));
        assert!(!run_receipt.to_string().contains("provider-secret"));

        let status_script = home.path().join("status.script");
        fs::write(&status_script, "/status\n/quit\n").unwrap();
        let status = base_cmd(home.path())
            .args([
                "--jsonl",
                "chat",
                "--no-session",
                "--script",
                &status_script.to_string_lossy(),
            ])
            .output()
            .expect("run product conformance status");
        assert!(status.status.success(), "{status:?}");
        let status_events = parse_product_jsonl(&status);
        let status_result = status_events
            .iter()
            .find(|event| event["type"] == "command_result" && event["result"]["kind"] == "status")
            .expect("structured status receipt");
        assert_eq!(status_result["result"]["provider_route"], *run_receipt);

        let offline_doctor = base_cmd(home.path())
            .args(["doctor", "--json"])
            .output()
            .expect("run offline product doctor");
        assert!(offline_doctor.status.success(), "{offline_doctor:?}");
        let offline_report: serde_json::Value =
            serde_json::from_slice(&offline_doctor.stdout).unwrap();
        assert_eq!(
            offline_report["provider_route"]["status"],
            "not_constructed_offline"
        );
        assert!(offline_report["provider_route"]["receipt"].is_null());

        let inference_doctor = base_cmd(home.path())
            .args(["doctor", "--inference", "--json"])
            .output()
            .expect("run product inference doctor");
        assert!(inference_doctor.status.success(), "{inference_doctor:?}");
        let inference_report: serde_json::Value =
            serde_json::from_slice(&inference_doctor.stdout).unwrap();
        assert_eq!(inference_report["provider_route"]["status"], "bound");
        assert_product_receipt(
            &inference_report["provider_route"]["receipt"],
            case,
            Some("product-model"),
        );

        write_provider_product_config(home.path(), &server.uri(), case, &fault_path);
        let fault = base_cmd(home.path())
            .args(["--jsonl", "ask", "exercise the product fault path"])
            .output()
            .expect("run product conformance fault");
        assert_eq!(fault.status.code(), Some(2), "{fault:?}");
        assert!(fault.stderr.is_empty(), "{fault:?}");
        let fault_events = parse_product_jsonl(&fault);
        let fault_run = fault_events
            .iter()
            .find(|event| event["type"] == "run_started")
            .expect("fault run_started");
        assert_eq!(fault_run["provider_route"], *run_receipt);
        let fault_attempt = fault_events
            .iter()
            .find(|event| event["type"] == "provider_attempt")
            .expect("fault provider attempt");
        assert_eq!(fault_attempt["category"], "authentication");
        assert_eq!(fault_attempt["status"], 401);
        assert_eq!(fault_attempt["terminal"], true);
        assert_eq!(
            fault_attempt["route_generation"],
            fault_run["provider_route"]["route"]["generation"]
        );
        assert!(!String::from_utf8_lossy(&fault.stdout).contains("provider-secret-body"));
        server.verify().await;
    }
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn both_compiled_adapters_correlate_product_cancellation_to_the_run_receipt() {
    use std::process::Stdio;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    for case in PROVIDER_PRODUCT_CASES {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(case.chat_path))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_secs(30))
                    .set_body_string(provider_product_sse(case)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let home = TempDir::new().expect("cancel conformance home");
        write_provider_product_config(home.path(), &server.uri(), case, case.chat_path);
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
        command
            .env("HOME", home.path())
            .env("KITSU_NO_PROJECT_CONTEXT", "1")
            .args(["--jsonl", "ask", "cancel the product request"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command
            .spawn()
            .expect("spawn cancellation conformance child");
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
        assert_eq!(output.status.code(), Some(130), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let events = parse_product_jsonl(&output);
        let run_started = events
            .iter()
            .find(|event| event["type"] == "run_started")
            .expect("cancel run_started");
        assert_product_receipt(&run_started["provider_route"], case, Some("product-model"));
        let attempt = events
            .iter()
            .find(|event| event["type"] == "provider_attempt")
            .expect("cancel provider attempt");
        assert_eq!(attempt["category"], "cancelled");
        assert_eq!(attempt["outcome"], "cancelled");
        assert_eq!(attempt["terminal"], true);
        assert_eq!(
            attempt["route_generation"],
            run_started["provider_route"]["route"]["generation"]
        );
        assert_eq!(events.last().unwrap()["type"], "final_summary");
        assert_eq!(events.last().unwrap()["status"], "cancelled");
        server.verify().await;
    }
}

#[tokio::test]
#[serial]
async fn native_adapter_rejects_model_catalog_before_any_network_send() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().expect("temp home");
    let kitsu_dir = home.path().join(".kitsu");
    fs::create_dir_all(&kitsu_dir).unwrap();
    fs::write(
        kitsu_dir.join("config.toml"),
        format!(
            r#"default_gateway = "native"

[gateways.native]
flavor = "anthropic"
adapter = "anthropic-messages"
base_url = "{}"
chat_path = "/v1/messages"
models_path = "/v1/models"
model = "native-model"
auth = {{ method = "none" }}
"#,
            server.uri()
        ),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.arg("models");
        cmd.assert().failure().stderr(predicate::str::contains(
            "anthropic-messages adapter does not implement model catalog discovery",
        ));
    })
    .await
    .expect("native catalog refusal process should join");

    assert!(server
        .received_requests()
        .await
        .expect("inspect requests")
        .is_empty());
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
        assert_eq!(report["provider_route"]["status"], "bound");
        assert_eq!(
            report["provider_route"]["receipt"]["model"],
            serde_json::Value::Null
        );
        assert_eq!(
            report["provider_route"]["receipt"]["route"]["capabilities"]["supported"],
            serde_json::json!(["model_catalog"])
        );

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
async fn test_doctor_online_catalog_does_not_require_a_configured_model() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "discoverable-model"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    let config = home.path().join("doctor-online-no-model.toml");
    fs::write(
        &config,
        format!(
            r#"default_gateway = "example"
[gateways.example]
flavor = "openai"
base_url = "{}"
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
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
            .expect("run catalog-only doctor without a model");
        // Doctor still reports the missing chat model as a readiness issue,
        // but that issue must not suppress the independent catalog probe.
        assert_eq!(output.status.code(), Some(1));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["online"]["status"], "reachable");
        assert_eq!(report["online"]["request_sent"], true);
        assert_eq!(report["online"]["models"], 1);
        assert_eq!(report["online"]["effective_model"], serde_json::Value::Null);
        assert_eq!(
            report["online"]["effective_model_available"],
            serde_json::Value::Null
        );
        assert!(!report["blocking_issues"].as_array().unwrap().is_empty());
    })
    .await
    .expect("catalog-only doctor process should join");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
    assert_eq!(requests[0].url.path(), "/v1/models");
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
        assert_eq!(report["provider_route"]["status"], "bound");
        assert_eq!(
            report["provider_route"]["receipt"]["route"]["adapter"],
            "openai_compatible"
        );
        assert_eq!(
            report["provider_route"]["receipt"]["route"]["wire_revision"],
            "openai_chat_completions_v1"
        );
        assert_eq!(
            report["provider_route"]["receipt"]["model"],
            "doctor-model"
        );

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
                "provider route: openai_compatible / openai_chat_completions_v1 / generation 1",
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
        .expect(3)
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
        assert_eq!(timing["outcome"], "succeeded");
        assert_eq!(timing["assistant"], "committed");
        let human_run_id = timing["run_id"].as_str().expect("human run identity");
        assert!(!human_run_id.is_empty());
        assert!(
            human_run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'_' | b'.' | b':')),
            "{timing}"
        );
        assert!(timing["duration_ms"].as_u64().unwrap() >= 35, "{timing}");

        let mut second_human = base_cmd(&home_path);
        let output = second_human
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--no-stream", "-p", "time this again"])
            .output()
            .expect("run second timed human prompt");
        assert!(output.status.success(), "{output:?}");
        let second_stderr = String::from_utf8(output.stderr).unwrap();
        let second_timing: serde_json::Value = second_stderr
            .lines()
            .last()
            .and_then(|line| line.strip_prefix("kitsu-timing: "))
            .map(serde_json::from_str)
            .transpose()
            .unwrap()
            .expect("second human terminal receipt");
        assert_ne!(
            second_timing["run_id"], timing["run_id"],
            "sequential process-runs must keep distinct identities"
        );
        assert_eq!(second_timing["outcome"], "succeeded");
        assert_eq!(second_timing["assistant"], "committed");

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
        assert_eq!(summary["outcome"], "succeeded");
        assert_eq!(summary["assistant"], "committed");
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

#[test]
fn test_help_exposes_yolo_as_the_dangerous_skip_permissions_alias() {
    let home = TempDir::new().unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--dangerously-skip-permissions"))
        .stdout(predicate::str::contains("--yolo"))
        .stdout(predicate::str::contains("DANGER"))
        .stdout(predicate::str::contains("Hard guards survive"));
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

/// EOF at an authority-admission surface interrupts the run before the
/// denial can be projected as a tool result into another provider request.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_interactive_confirmation_eof_interrupts_without_followup_send() {
    use std::io::Write as _;
    use wiremock::matchers::{method, path};
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
            let interruption_seen = output
                .windows(b"authority input closed".len())
                .any(|window| window == b"authority input closed");
            if eof_sent && !quit_sent && interruption_seen && prompt_count >= 2 {
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
        transcript.contains("authority input closed"),
        "{transcript}"
    );
    assert!(
        transcript.contains("interrupted") && transcript.contains("assistant discarded"),
        "interactive process receipt did not retain the authority interruption: {transcript}"
    );
    assert!(
        !transcript.contains("invalid permission response"),
        "EOF is an interruption, not malformed consent: {transcript}"
    );
    assert!(!target.exists(), "confirmation EOF performed the write");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "EOF must prevent a follow-up send");
    server.verify().await;
}

/// An explicit `n` is still an ordinary policy denial: no effect occurs, and
/// the provider may inspect the bounded tool error and recover next round.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_interactive_explicit_denial_remains_provider_recoverable() {
    use std::io::Write as _;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let repo = TempDir::new().expect("temp interactive repo");
    let target = repo.path().join("must-not-exist.txt");
    let write_arguments = serde_json::json!({
        "path": target.to_string_lossy(),
        "content": "explicit denial must not write"
    })
    .to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "EXPLICIT_DENIAL_RECOVERED"
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
                        "id": "explicit-denial-write",
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
        let mut denial_sent = false;
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
                && !denial_sent
                && output
                    .windows(b"permission> kitsu wants to write".len())
                    .any(|window| window == b"permission> kitsu wants to write")
            {
                terminal.write_all(b"n\n").expect("deny guarded write");
                terminal.flush().unwrap();
                denial_sent = true;
            }
            let recovered = output
                .windows(b"EXPLICIT_DENIAL_RECOVERED".len())
                .any(|window| window == b"EXPLICIT_DENIAL_RECOVERED");
            if denial_sent && !quit_sent && recovered && prompt_count >= 2 {
                terminal.write_all(b"/quit\n").expect("exit chat");
                terminal.flush().unwrap();
                quit_sent = true;
            }
        })
    })
    .await
    .expect("interactive explicit-denial fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        transcript.contains("EXPLICIT_DENIAL_RECOVERED"),
        "{transcript}"
    );
    assert!(
        transcript.contains("succeeded") && transcript.contains("assistant committed"),
        "recovered denial did not publish a committed interactive aggregate: {transcript}"
    );
    assert!(
        !transcript.contains("authority input closed"),
        "{transcript}"
    );
    assert!(!target.exists(), "explicit denial performed the write");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "explicit denial should allow recovery");
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
    assert!(
        transcript.contains("cancelled") && transcript.contains("assistant discarded"),
        "queued exit cancellation did not publish a cancelled aggregate: {transcript}"
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
        assert_eq!(events[2]["route_generation"], 1);
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
        assert_eq!(events[2]["route_generation"], 1);
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

    let attempt_routes = events
        .iter()
        .filter(|event| event["type"] == "provider_attempt")
        .map(|event| {
            (
                event["purpose"].as_str().unwrap().to_string(),
                event["route_generation"].as_u64(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        attempt_routes,
        [
            ("inference".to_string(), Some(1)),
            ("inference".to_string(), Some(2)),
        ]
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

/// `/retry` is a new logical operation, not a hidden replay through the failed
/// request's old route. After `/model` publishes a successor, the retained
/// prompt must be sent once through that successor and its attempt receipt must
/// identify the new generation.
#[tokio::test]
#[serial]
async fn test_retry_after_model_switch_uses_the_successor_generation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let responder_attempts = attempts.clone();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_request: &wiremock::Request| {
            if responder_attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": {"message": "deliberate first-operation failure"}
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "retry used successor"},
                        "finish_reason": "stop"
                    }]
                }))
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("retry-route-successor.script");
    fs::write(
        &script,
        "original replay prompt\n/model retry-model\n/retry\n/quit\n",
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
                "--context-window",
                "8192",
                "chat",
                "--no-session",
                "--no-tools",
                "--allow-turn-errors",
                "--script",
                &script_path,
            ])
            .output()
            .expect("run retry route-generation script")
    })
    .await
    .expect("retry route-generation command joins");

    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let attempt_routes = events
        .iter()
        .filter(|event| event["type"] == "provider_attempt" && event["purpose"] == "inference")
        .map(|event| {
            (
                event["route_generation"].as_u64(),
                event["outcome"].as_str().unwrap().to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        attempt_routes,
        [
            (Some(1), "rejected".to_string()),
            (Some(2), "body_completed".to_string()),
        ]
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let bodies = requests
        .iter()
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bodies[0]["model"], "example-model-v1");
    assert_eq!(bodies[1]["model"], "retry-model");
    for body in &bodies {
        assert!(body["messages"].as_array().unwrap().iter().any(|message| {
            message["role"] == "user" && message["content"] == "original replay prompt"
        }));
    }
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_failed_model_publication_keeps_the_old_route_usable() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "old route retained"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let oversized = "m".repeat(4097);
    let script = home.path().join("failed-model-publication.script");
    fs::write(
        &script,
        format!("/model {oversized}\nafter failed switch\n/quit\n"),
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    let script_path = script.to_string_lossy().into_owned();

    tokio::task::spawn_blocking(move || {
        base_cmd(&home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--gateway",
                "example",
                "--no-stream",
                "--context-window",
                "8192",
                "chat",
                "--no-session",
                "--no-tools",
                "--allow-turn-errors",
                "--script",
                &script_path,
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("old route retained"))
            .stderr(predicate::str::contains("could not publish model switch"));
    })
    .await
    .expect("failed-publication command joins");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "failed switch caused an auxiliary send");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "example-model-v1");
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
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
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
    assert!(
        transcript.contains("provider_fault") && transcript.contains("assistant discarded"),
        "empty response did not publish the failed interactive aggregate: {transcript}"
    );
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

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_interactive_recovered_failed_turn_publishes_partial_process_outcome() {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let responder_attempts = attempts.clone();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |_request: &wiremock::Request| {
            let content = if responder_attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                " \n\t"
            } else {
                "RECOVERED_INTERACTIVE_TURN"
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

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let repo = TempDir::new().expect("temp interactive repo");
    let repo_path = repo.path().to_path_buf();
    let (status, transcript) = tokio::task::spawn_blocking(move || {
        let mut prompt_sent = false;
        let mut retry_sent = false;
        let mut quit_sent = false;
        run_plain_interactive_chat_pty(
            &home_path,
            &repo_path,
            &["--no-tools"],
            |output, terminal| {
                let prompt_count = output
                    .windows(b"kitsu>".len())
                    .filter(|window| *window == b"kitsu>")
                    .count();
                if !prompt_sent && prompt_count >= 1 {
                    terminal
                        .write_all(b"FIRST_TURN_IS_EMPTY\n")
                        .expect("submit first interactive prompt");
                    terminal.flush().unwrap();
                    prompt_sent = true;
                }
                let rolled_back = output
                    .windows(b"turn rolled back - /retry".len())
                    .any(|window| window == b"turn rolled back - /retry");
                if prompt_sent && !retry_sent && rolled_back && prompt_count >= 2 {
                    terminal.write_all(b"/retry\n").expect("retry failed turn");
                    terminal.flush().unwrap();
                    retry_sent = true;
                }
                let recovered = output
                    .windows(b"RECOVERED_INTERACTIVE_TURN".len())
                    .any(|window| window == b"RECOVERED_INTERACTIVE_TURN");
                if retry_sent && !quit_sent && recovered && prompt_count >= 3 {
                    terminal.write_all(b"/quit\n").expect("exit recovered chat");
                    terminal.flush().unwrap();
                    quit_sent = true;
                }
            },
        )
    })
    .await
    .expect("interactive recovered-fault fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        transcript.contains("RECOVERED_INTERACTIVE_TURN"),
        "{transcript}"
    );
    assert!(
        transcript.contains("partially_succeeded") && transcript.contains("assistant committed"),
        "process aggregate collapsed to its last successful turn: {transcript}"
    );
    assert_eq!(attempts.load(Ordering::Acquire), 2);
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
        assert_eq!(timing["outcome"], "provider_fault");
        assert_eq!(timing["assistant"], "discarded");
        assert!(timing["run_id"].as_str().is_some_and(|id| !id.is_empty()));

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
            .expect(2)
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
            .expect(2)
            .mount(&server)
            .await;

        let home = TempDir::new().unwrap();
        let allowed = TempDir::new().unwrap();
        write_config(home.path(), &server.uri());
        let script = home.path().join(format!("{expected_kind}.script"));
        fs::write(&script, "exercise the tool\n").unwrap();
        let home_path = home.path().to_path_buf();
        let allowed_path = allowed.path().to_path_buf();

        let human_home = home_path.clone();
        let human_allowed = allowed_path.clone();
        let human_script = script.clone();
        let human = tokio::task::spawn_blocking(move || {
            let mut cmd = base_cmd(&human_home);
            cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
                .args([
                    "--no-stream",
                    "chat",
                    "--no-session",
                    "--allow-path",
                    &human_allowed.to_string_lossy(),
                    "--script",
                    &human_script.to_string_lossy(),
                ])
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(human.status.code(), Some(1), "{human:?}");
        let human_stderr = String::from_utf8(human.stderr).unwrap();
        let human_timing: serde_json::Value = human_stderr
            .lines()
            .last()
            .and_then(|line| line.strip_prefix("kitsu-timing: "))
            .map(serde_json::from_str)
            .transpose()
            .unwrap()
            .expect("strict human run terminal receipt");
        assert_eq!(human_timing["outcome"], "tool_fault");
        assert_eq!(human_timing["assistant"], "discarded");

        let output = tokio::task::spawn_blocking(move || {
            let mut cmd = base_cmd(&home_path);
            cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
                .args([
                    "--jsonl",
                    "--no-stream",
                    "chat",
                    "--no-session",
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
    assert!(
        tool_content.contains("bytes omitted by artifact-free policy"),
        "the artifact-free bound marker must reach the provider in the second request, \
         but the tool message there is {} bytes, beginning {:?}. An elided or shrunken \
         tool message means history trimming dropped it before the provider saw it, which \
         is what happens when the protected prompt (system instructions plus tool schemas) \
         grows into the window budget - check whether a tool schema grew. The tool ran \
         and bounded its own output (artifact_status {}, output_truncated {}, \
         output_bytes {}, output_retained_bytes {}), so the loss is downstream of the \
         policy. The second request carried {} messages.",
        tool_content.len(),
        tool_content.chars().take(300).collect::<String>(),
        finished["artifact_status"],
        finished["output_truncated"],
        finished["output_bytes"],
        finished["output_retained_bytes"],
        second["messages"].as_array().unwrap().len(),
    );
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
        .stderr(predicate::str::contains("status.adapter:"))
        .stderr(predicate::str::contains("context.window tokens:"))
        .stderr(predicate::str::contains("project ctx").not())
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
    assert_eq!(events[3]["route_generation"], 1);
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

/// `/status` and `/permissions` expose typed route and authority facts without
/// making a model request.
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
            .stderr(predicate::str::contains(
                "status.adapter: openai_compatible",
            ))
            .stderr(predicate::str::contains("status.model: test-model"))
            .stderr(predicate::str::contains("status.generation: 1"))
            .stderr(predicate::str::contains(
                "status.wire revision: openai_chat_completions_v1",
            ))
            .stderr(predicate::str::contains("context.occupancy:"))
            .stderr(predicate::str::contains("permissions:"))
            .stderr(predicate::str::contains("mode         deny"))
            .stderr(predicate::str::contains("read roots   none"))
            .stderr(predicate::str::contains("write roots  none"));
    })
    .await
    .expect("join");
}

/// A redirected `/status` is a local observation only: it renders the typed
/// deck without terminal escapes and cannot turn into a provider request.
#[tokio::test]
#[serial]
async fn test_status_redirected_deck_is_offline_and_includes_runtime_identity() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("status.txt");
    fs::write(&script, "/status\n/quit\n").unwrap();
    let home_path = home.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        base_cmd(&home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .current_dir(&home_path)
            .args([
                "--gateway",
                "example",
                "chat",
                "--no-session",
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .expect("redirected status")
    })
    .await
    .expect("join redirected status");

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(!text.contains('\u{1b}'), "{text}");
    for required in [
        "status.version:",
        "status.cwd:",
        "status.adapter: openai_compatible",
        "status.model: example-model-v1",
        "status.generation: 1",
        "status.run:",
        "status.authority: deny",
        "context.occupancy:",
        "usage.coverage:",
    ] {
        assert!(text.contains(required), "missing {required:?}: {text}");
    }
    assert!(
        text.contains(home.path().to_string_lossy().as_ref()),
        "cwd missing from typed deck: {text}"
    );
    for unavailable_account_fact in ["email", "credits", "rate limit", "price"] {
        assert!(
            !text.to_ascii_lowercase().contains(unavailable_account_fact),
            "invented {unavailable_account_fact}: {text}"
        );
    }
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "/status sent a provider request"
    );
}

#[test]
#[serial]
fn test_status_rejects_nonempty_arguments_with_registered_syntax() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("status-args.txt");
    fs::write(&script, "/status unexpected\n").unwrap();

    base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args([
            "--gateway",
            "example",
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("usage: /status"))
        .stderr(predicate::str::contains("status.adapter:").not());
}

#[cfg(unix)]
#[test]
#[serial]
fn test_status_styled_deck_uses_narrow_layout_on_a_thirty_two_column_pty() {
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("narrow-status.txt");
    fs::write(&script, "/status\n/quit\n").unwrap();

    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 40,
        ws_col: 32,
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
                std::ptr::null_mut(),
                &mut size,
            )
        },
        0,
        "allocate narrow status PTY"
    );
    // SAFETY: the live master descriptor remains owned by this test.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );

    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("kitsu"));
    command
        .env("HOME", home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .env("KITSU_NO_PROJECT_CONTEXT", "1")
        .env("TERM", "xterm-256color")
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .env_remove("NO_COLOR")
        .env_remove("KITSU_PLAIN")
        .env_remove("KITSU_SCREEN_READER")
        .current_dir(home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // SAFETY: ownership of the live slave descriptor transfers once.
        .stderr(unsafe { Stdio::from_raw_fd(slave) })
        .args([
            "--gateway",
            "example",
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ]);
    let mut child = command.spawn().expect("spawn narrow status script");
    // SAFETY: ownership of the remaining master descriptor transfers once.
    let mut terminal = unsafe { fs::File::from_raw_fd(master) };
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        let mut buffer = [0_u8; 4096];
        match terminal.read(&mut buffer) {
            Ok(0) => {}
            Ok(read) => output.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("read narrow status PTY: {error}"),
        }
        if let Some(status) = child.try_wait().expect("poll narrow status script") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "narrow status script timed out: {}",
                String::from_utf8_lossy(&output)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    loop {
        let mut buffer = [0_u8; 4096];
        match terminal.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => output.extend_from_slice(&buffer[..read]),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == Some(libc::EIO) =>
            {
                break;
            }
            Err(error) => panic!("drain narrow status PTY: {error}"),
        }
    }

    let text = kitsu_render::sanitize_terminal_text(&String::from_utf8_lossy(&output));
    assert!(status.success(), "{text}");
    for required in ["[status]", "version", "cwd", "[context]", "[usage]"] {
        assert!(text.contains(required), "missing {required:?}: {text}");
    }
    assert!(
        !text.contains("git          "),
        "legacy Git formatter mounted: {text}"
    );
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
        "/status\n/permissions\n/context\n/sessions\n/skills\n/jobs\n/model alternate-model\n/status\n/compact\n/quit\n",
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
    assert_eq!(results.len(), 9);
    let run_started = events
        .iter()
        .find(|event| event["type"] == "run_started")
        .expect("run_started route receipt");
    assert_eq!(results[0]["result"]["kind"], "status");
    assert_eq!(results[0]["result"]["session"]["model"], "example-model-v1");
    assert!(results[0]["result"]["session"]["cwd"].is_string());
    assert!(results[0]["result"]["context"]["effective"].is_number());
    assert!(results[0]["result"]["conversation"]["messages"].is_number());
    assert!(results[0]["result"]["tools"]["names"].is_array());
    assert_eq!(
        results[0]["result"]["provider_route"]["route"]["adapter"],
        "openai_compatible"
    );
    assert_eq!(
        results[0]["result"]["provider_route"]["route"]["generation"],
        run_started["provider_route"]["route"]["generation"]
    );
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
    assert_eq!(results[7]["result"]["kind"], "status");
    assert_eq!(results[7]["result"]["session"]["model"], "alternate-model");
    assert_eq!(
        results[7]["result"]["provider_route"]["route"]["generation"],
        2
    );
    assert_eq!(
        results[7]["result"]["provider_route"]["model"],
        "alternate-model"
    );
    assert_eq!(results[8]["result"]["kind"], "compaction");
    assert!(results[8]["result"]["changed"].is_boolean());
    assert!(results[8]["result"]["tokens_before"].is_number());
    assert!(results[8]["result"]["tokens_after"].is_number());
    assert!(results[8]["result"]["usage"].is_object());
    let started = events
        .iter()
        .filter(|event| event["type"] == "command_started")
        .count();
    let finished = events
        .iter()
        .filter(|event| event["type"] == "command_finished")
        .count();
    assert_eq!(started, 10);
    assert_eq!(finished, started, "every slash frame must close");
}

#[test]
#[serial]
fn test_status_jsonl_keeps_legacy_shape_and_uses_the_same_typed_observation() {
    fn sorted_keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys = value
            .as_object()
            .expect("JSONL status member is an object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }

    fn assert_keys(value: &serde_json::Value, expected: &[&str]) {
        let mut expected = expected
            .iter()
            .map(|key| (*key).to_string())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(sorted_keys(value), expected);
    }

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("status-jsonl.txt");
    fs::write(&script, "/status\n/quit\n").unwrap();
    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(home.path())
        .args([
            "--gateway",
            "example",
            "--jsonl",
            "chat",
            "--no-session",
            "--script",
            &script.to_string_lossy(),
        ])
        .output()
        .expect("JSONL status observation");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");

    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let status = &events
        .iter()
        .find(|event| event["type"] == "command_result" && event["result"]["kind"] == "status")
        .expect("status command result")["result"];

    assert_keys(
        status,
        &[
            "agents",
            "artifact_policy",
            "commands",
            "compaction",
            "context",
            "conversation",
            "kind",
            "permissions",
            "provider_route",
            "session",
            "text",
            "tools",
            "version",
        ],
    );
    assert_keys(
        &status["session"],
        &[
            "auth_status",
            "autosave_dirty",
            "autosave_enabled",
            "config_path",
            "cwd",
            "endpoint",
            "gateway",
            "id",
            "model",
            "project_context",
            "reasoning_effort",
            "state_dir",
        ],
    );
    assert_keys(
        &status["context"],
        &[
            "cache",
            "configured_window",
            "discovery",
            "effective",
            "remaining",
            "source",
            "used",
            "window",
        ],
    );
    assert_keys(
        &status["conversation"],
        &["assistant_turns", "messages", "tool_results", "user_turns"],
    );
    assert_keys(
        &status["compaction"],
        &[
            "exchanges_removed",
            "runs",
            "tokens_saved",
            "tool_results_elided",
        ],
    );
    assert_keys(&status["tools"], &["count", "names"]);
    assert_keys(
        &status["permissions"],
        &["behavior", "mode", "read_roots", "write_roots"],
    );
    assert_keys(
        &status["commands"],
        &["cwd_roots", "enabled", "exact_grants", "mode"],
    );
    assert_keys(
        &status["agents"],
        &["cancelled", "done", "failed", "running", "total"],
    );

    // `text` intentionally changes presentation to the deterministic
    // redirected deck. Its overlapping facts must be the same observation as
    // the legacy structured fields, rather than a second calculation.
    let text = status["text"].as_str().expect("status text");
    let text_without_grouping = text.replace(',', "");
    for expected in [
        format!("status.version: {}", status["version"].as_str().unwrap()),
        format!("status.cwd: {}", status["session"]["cwd"].as_str().unwrap()),
        format!(
            "status.model: {}",
            status["session"]["model"].as_str().unwrap()
        ),
        format!(
            "status.generation: {}",
            status["provider_route"]["route"]["generation"]
                .as_u64()
                .unwrap()
        ),
        format!(
            "context.effective tokens: {}",
            status["context"]["effective"].as_u64().unwrap()
        ),
        format!(
            "context.used tokens: {}",
            status["context"]["used"].as_u64().unwrap()
        ),
        format!(
            "status.read roots: {}",
            status["permissions"]["read_roots"].as_u64().unwrap()
        ),
        format!(
            "status.write roots: {}",
            status["permissions"]["write_roots"].as_u64().unwrap()
        ),
    ] {
        let expected_without_grouping = expected.replace(',', "");
        assert!(
            text_without_grouping.contains(&expected_without_grouping),
            "typed text disagrees with JSONL field {expected:?}: {text}"
        );
    }
    assert!(text.contains("status.authority: deny"), "{text}");
    for forbidden_key in ["account", "email", "credits", "rate_limits", "prices"] {
        assert!(
            status.get(forbidden_key).is_none(),
            "invented {forbidden_key}"
        );
    }
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

/// Card **F67-print**: `/jobs prune <id>` prints an unviewed result before it
/// discards the record, so the interactive path stops being the only one that
/// loses the work.
///
/// The asymmetry F67 records: `kitsu agents --json` / automation gets
/// `terminal_job_payload(&status)` in the reply, while an interactive human got
/// only `~ pruned background job #1` on stderr -- and `AgentRegistry::prune`
/// removes the record, which is the one thing
/// `take_unobserved_terminal_results` cannot see, so the exit guard could not
/// surface it afterwards either. The result was simply gone.
///
/// The decision is to print, **then** prune -- not to refuse. So this pins
/// BOTH halves, and the second half is what keeps the card from being "fixed"
/// by turning it into a refusal:
///
/// 1. the result body reaches stderr, once, before the prune receipt, and
/// 2. the record is really gone afterwards -- `/jobs` shows an empty deck.
///
/// Driven end to end rather than at a seam because the loss is a property of
/// what the human sees on stderr, and stderr is the surface F67 is about.
#[tokio::test]
#[serial]
async fn test_script_jobs_prune_prints_an_unviewed_result_before_discarding_the_record() {
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
                "message": {"role": "assistant", "content": "PRUNE_RESULT_SENTINEL"},
                "finish_reason": "stop"
            }]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    // The foreground turn is held open long enough for the detached agent to
    // finish, so `/jobs prune 1` on the next script line meets a terminal
    // record whose handoff is still `Ready`. A still-running agent would be
    // refused with "still running", which is asserted against below rather
    // than passing silently.
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"role\":\"tool\""))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(500))
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
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let script = home.path().join("prune-unviewed.script");
    fs::write(&script, "start detached work\n/jobs prune 1\n/jobs\n").unwrap();
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
    assert!(
        output.status.success(),
        "stderr: {stderr}\nstdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    // The precondition, loudly: the prune really met a finished record. If the
    // detached agent were still running the prune would have been refused and
    // every assertion below would be about the wrong thing.
    assert!(
        !stderr.contains("is still running; stop it first"),
        "the detached agent must be terminal when /jobs prune runs: {stderr}"
    );

    let pruned_at = stderr
        .find("~ pruned background job #1")
        .unwrap_or_else(|| panic!("the prune receipt is still printed: {stderr}"));
    let result_at = stderr.find("PRUNE_RESULT_SENTINEL").unwrap_or_else(|| {
        panic!(
            "F67: /jobs prune must print the unviewed result instead of \
             destroying it, as `agents --json` already keeps it: {stderr}"
        )
    });
    assert!(
        result_at < pruned_at,
        "the result is printed, then the prune is reported: {stderr}"
    );
    // The header `print_background_results` already gives an unobserved
    // result, measured rather than invented: the id and the task, with the
    // body beneath it.
    assert!(
        stderr.contains("background job #1 result"),
        "the printed result is headed the way the exit guard heads one: {stderr}"
    );
    assert_eq!(
        stderr.matches("PRUNE_RESULT_SENTINEL").count(),
        1,
        "printed exactly once -- the prune prints it and the record is then \
         gone, so shutdown cannot print it again: {stderr}"
    );

    // And the prune still happened. This is the half that fails if the card is
    // "fixed" by refusing to prune an unviewed result (option three), which is
    // the `x` key's answer and deliberately not this command's.
    assert!(
        stderr[pruned_at..].contains("no background agents"),
        "the record is really gone: /jobs after the prune shows an empty \
         deck, so printing did not become a refusal: {stderr}"
    );
    assert!(
        stderr[pruned_at..].contains("0 total"),
        "and the deck counts agree -- the row is gone, not merely unlisted: \
         {stderr}"
    );
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
    assert_eq!(
        output.status.code(),
        Some(0),
        "kitsu must exit 0; stderr: {}; stdout error events: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.contains("\"type\":\"error\""))
            .collect::<Vec<_>>()
            .join(" | ")
    );
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
    assert_eq!(
        output.status.code(),
        Some(0),
        "kitsu must exit 0; stderr: {}; stdout error events: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.contains("\"type\":\"error\""))
            .collect::<Vec<_>>()
            .join(" | ")
    );
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
    let big_prompt = "review this attachment ".repeat(900).replace('\n', " ");
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
    assert_eq!(
        output.status.code(),
        Some(0),
        "kitsu must exit 0; stderr: {}; stdout error events: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.contains("\"type\":\"error\""))
            .collect::<Vec<_>>()
            .join(" | ")
    );
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
    assert_eq!(
        output.status.code(),
        Some(0),
        "kitsu must exit 0; stderr: {}; stdout error events: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.contains("\"type\":\"error\""))
            .collect::<Vec<_>>()
            .join(" | ")
    );

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

// --- T2-S5a: agents.blocking / agents.block_timeout_secs -------------------
//
// FAILING-FIRST. `[agents] blocking` and `[agents] block_timeout_secs` do not
// exist yet, and `AgentsConfig` is `deny_unknown_fields`, so every run below
// that writes them currently exits 1 with `unknown field`.
//
// These are the only tests in the tree that drive the WHOLE blocked path --
// real gate, real prompt route, real registry, real timer -- because
// `kitsu_tools::ask_registry` is `pub(crate)` and `Mode::Ask` is reached only
// by an interactive (PTY) run. A `--script` run is non-interactive and gets
// `Mode::Deny`, which never consults the prompt route at all.

/// Like `write_config`, plus an `[agents]` table written verbatim.
fn write_config_with_agents_table(home: &Path, base_url: &str, agents: &str) {
    write_config(home, base_url);
    let path = home.join(".kitsu").join("config.toml");
    let mut config = fs::read_to_string(&path).expect("read config.toml");
    config.push_str("\n[agents]\n");
    config.push_str(agents);
    fs::write(&path, config).expect("write config.toml");
}

/// One mock gateway that drives: parent spawns a detached agent, the detached
/// agent reads a path outside every allow root, and whatever the gate hands
/// back ends the child's turn.
#[cfg(unix)]
async fn mount_gated_detached_agent_gateway(server: &wiremock::MockServer, forbidden: &Path) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    let forbidden = forbidden.to_string_lossy().into_owned();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(move |request: &wiremock::Request| {
            let body = String::from_utf8_lossy(&request.body).into_owned();
            // Only the parent is offered the spawn tools (a detached agent is
            // depth-1), so the advertised tool name -- not the task text,
            // which the parent's own tool_call echoes -- tells them apart.
            let is_child = !body.contains("spawn_agent_bg");
            let after_tool = body.contains("\"role\":\"tool\"");
            // What the gate handed the child, so the child's answer carries it
            // into the registry where `/agents` can print it.
            let gate_answer = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("messages")?
                        .as_array()?
                        .iter()
                        .rev()
                        .find(|message| message.get("role").and_then(|r| r.as_str()) == Some("tool"))?
                        .get("content")?
                        .as_str()
                        .map(str::to_string)
                })
                .unwrap_or_default();
            let json = match (is_child, after_tool) {
                // The detached agent's first round: touch the gate.
                (true, false) => serde_json::json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "gated-read-1",
                                "type": "function",
                                "function": {
                                    "name": "fs_read_file",
                                    "arguments": format!(
                                        "{{\"path\":{}}}",
                                        serde_json::Value::String(forbidden.clone())
                                    )
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                }),
                // The detached agent got an answer from the gate.
                (true, true) => serde_json::json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": format!("CHILD_SAW: {gate_answer}")
                        },
                        "finish_reason": "stop"
                    }]
                }),
                // The parent, after its spawn tool returned.
                (false, true) => serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "PARENT_DELEGATED"},
                        "finish_reason": "stop"
                    }]
                }),
                // The parent's first round: delegate.
                (false, false) => serde_json::json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "parent-spawn-1",
                                "type": "function",
                                "function": {
                                    "name": "spawn_agent_bg",
                                    "arguments": "{\"task\":\"GATED_CHILD_TASK: read the file you were given\"}"
                                }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                }),
            };
            ResponseTemplate::new(200).set_body_json(json)
        })
        .mount(server)
        .await;
}

/// Drive the interactive PTY: delegate once, then poll `/jobs` until both
/// `wanted` substrings have been seen (or the harness deadline fires), then
/// quit. Polling `/jobs` from the idle prompt is the only synchronization
/// used, so there is no sleep-and-hope anywhere.
#[cfg(unix)]
fn run_gated_delegation_pty(
    home: &Path,
    cwd: &Path,
    extra_chat_args: &[&str],
) -> (std::process::ExitStatus, String) {
    use std::io::Write as _;

    let mut delegated = false;
    let mut quit = false;
    let mut polls = 0_u32;
    let mut last_poll = std::time::Instant::now();
    run_plain_interactive_chat_pty(home, cwd, extra_chat_args, move |output, terminal| {
        let text = String::from_utf8_lossy(output);
        let prompts = text.matches("kitsu>").count();
        if !delegated && prompts >= 1 {
            terminal
                .write_all(b"delegate the read\n")
                .expect("delegate");
            terminal.flush().unwrap();
            delegated = true;
            last_poll = std::time::Instant::now();
            return;
        }
        if !delegated || quit {
            return;
        }
        if !text.contains("PARENT_DELEGATED") {
            return;
        }
        // Stop as soon as the detached agent's outcome is observable, or after
        // a bounded number of polls, so a missing outcome fails on its own
        // assertion rather than on the harness deadline.
        let settled = text.contains("CHILD_SAW") || text.contains("interrupted");
        if settled || polls >= 20 {
            terminal.write_all(b"/quit\n").expect("quit");
            terminal.flush().unwrap();
            quit = true;
            return;
        }
        if last_poll.elapsed() >= std::time::Duration::from_millis(150) {
            // `/jobs view` -- NOT bare `/jobs`. The `needs input` heading is
            // produced only by `format_jobs_view`; the bare command prints the
            // agent deck, which never contains that string. Polling the deck
            // made the positive assertion unreachable at any timeout and made
            // the two negative `!contains("needs input")` guards pass
            // vacuously. `/agents` prints finished results.
            terminal.write_all(b"/jobs view\n/agents\n").expect("poll");
            terminal.flush().unwrap();
            polls += 1;
            last_poll = std::time::Instant::now();
        }
    })
}

/// THE DEFAULT (B1, N1, plan row T2-S5): with no `[agents]` table at all, an
/// interactive run is unchanged -- the detached agent's gate still fails
/// closed with today's message, nothing is ever parked, and `/jobs` never
/// grows a `needs input` group.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_default_blocking_fails_a_gated_detached_agent_closed() {
    let server = wiremock::MockServer::start().await;
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let outside = TempDir::new().expect("a directory no allow root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").unwrap();
    mount_gated_detached_agent_gateway(&server, &forbidden).await;
    write_config(home.path(), &server.uri());

    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let allow = repo_path.to_string_lossy().into_owned();
    let (status, transcript) = tokio::task::spawn_blocking(move || {
        run_gated_delegation_pty(&home_path, &repo_path, &["--allow-path", &allow])
    })
    .await
    .expect("interactive gated-delegation fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        transcript.contains("CHILD_SAW"),
        "the detached agent must have reached the gate and reported back: {transcript}"
    );
    assert!(
        transcript.contains("background agents cannot open interactive permission prompts"),
        "the default must keep today's exact fail-closed message: {transcript}"
    );
    assert!(
        !transcript.contains("needs input"),
        "nothing may park under the default: {transcript}"
    );
}

/// Explicitly `fail_closed` must be indistinguishable from an absent table.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_explicit_fail_closed_is_indistinguishable_from_an_absent_agents_table() {
    let server = wiremock::MockServer::start().await;
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let outside = TempDir::new().expect("a directory no allow root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").unwrap();
    mount_gated_detached_agent_gateway(&server, &forbidden).await;
    write_config_with_agents_table(home.path(), &server.uri(), "blocking = \"fail_closed\"\n");

    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let allow = repo_path.to_string_lossy().into_owned();
    let (status, transcript) = tokio::task::spawn_blocking(move || {
        run_gated_delegation_pty(&home_path, &repo_path, &["--allow-path", &allow])
    })
    .await
    .expect("interactive gated-delegation fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        !transcript.contains("unknown field"),
        "`blocking` must be an accepted key: {transcript}"
    );
    assert!(
        transcript.contains("background agents cannot open interactive permission prompts"),
        "explicit fail_closed is today's behaviour: {transcript}"
    );
    assert!(!transcript.contains("needs input"), "{transcript}");
}

/// Scripted and JSONL runs must be byte-identical under the default. Both
/// configurations are run twice; the two configurations' transcripts must
/// match each other exactly, which also proves the scenario is deterministic.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_scripted_and_jsonl_runs_are_byte_identical_under_the_default() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"content\":\"BYTE_IDENTICAL_TASK\""))
        // THE DETERMINISM OF THIS TEST LIVES HERE (F39, eight occurrences).
        // Without this delay the detached agent and the foreground turn race:
        // the agent needs one round trip to this very mock, the foreground
        // needs one more, and whichever lands first decides whether
        // `registry.running()` is still non-zero when input ends. On the
        // running side the run narrates `input ended with 1 background
        // agent(s) running; joining them` and `joined 1 background agent(s)
        // before exit`; on the finished side it narrates neither -- and the
        // byte comparison below then fails on two lines of correct product
        // output. The delay does not replace the wait: it only puts the
        // detached completion behind end-of-input, so the exit join is always
        // entered and then WAITS on the agent registry's watch channel until
        // the agent is terminal (`join_background_agents_at_exit`). That is
        // the synchronisation; one second against an end-of-input that
        // arrives in single-digit milliseconds is the ordering, and it is far
        // under the 15s first join heartbeat, which would otherwise add a
        // `still waiting` line of its own.
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(1))
                .set_body_json(serde_json::json!({
                    "choices": [{
                        "message": {"role": "assistant", "content": "DETACHED_DONE"},
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
                "message": {"role": "assistant", "content": "FOREGROUND_DONE"},
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
                        "id": "byte-identical-1",
                        "type": "function",
                        "function": {
                            "name": "spawn_agent_bg",
                            "arguments": "{\"task\":\"BYTE_IDENTICAL_TASK\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .with_priority(10)
        .mount(&server)
        .await;

    let uri = server.uri();
    let transcripts = tokio::task::spawn_blocking(move || {
        let run = |agents: Option<&str>, jsonl: bool| {
            let home = TempDir::new().expect("temp home");
            match agents {
                None => write_config(home.path(), &uri),
                Some(table) => write_config_with_agents_table(home.path(), &uri, table),
            }
            let script = home.path().join("byte-identical.script");
            fs::write(&script, "delegate\n").unwrap();
            let mut cmd = base_cmd(home.path());
            cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN).arg("--no-stream");
            if jsonl {
                cmd.arg("--jsonl");
            }
            cmd.args([
                "--gateway",
                "example",
                "chat",
                "--no-session",
                "--script",
                &script.to_string_lossy(),
            ]);
            let output = cmd.output().unwrap();
            // The temp home is the only per-run varying token.
            let home_token = home.path().to_string_lossy().into_owned();
            // Per-run ids and millisecond timings are the only legitimately
            // varying tokens, so every digit run collapses to `N`. A single
            // changed word, line, order or event still fails the comparison.
            let clean = |bytes: &[u8]| {
                // The JSONL `run_started` event carries
                // `provider_route.route.profile`, a `profile-v1-` + 32 hex
                // digest of the CANONICALISED CONFIG PATH. Each run here uses
                // a fresh temp home, so that field differs by construction --
                // and the digit-collapsing below cannot hide it, because a
                // hex digest's `a`-`f` survive. It is environmental like
                // `run_id` and `duration_ms`, not behaviour, so it is
                // normalised rather than compared.
                let text = String::from_utf8_lossy(bytes).replace(&home_token, "<HOME>");
                let text = {
                    let mut out = String::with_capacity(text.len());
                    let mut rest = text.as_str();
                    while let Some(at) = rest.find("profile-v1-") {
                        out.push_str(&rest[..at]);
                        out.push_str("profile-v1-<DIGEST>");
                        let after = &rest[at + "profile-v1-".len()..];
                        let skip = after
                            .char_indices()
                            .take_while(|(i, c)| *i < 32 && c.is_ascii_hexdigit())
                            .count();
                        rest = &after[skip..];
                    }
                    out.push_str(rest);
                    out
                };
                let mut out = String::with_capacity(text.len());
                let mut in_digits = false;
                for ch in text.chars() {
                    if ch.is_ascii_digit() {
                        if !in_digits {
                            out.push('N');
                            in_digits = true;
                        }
                    } else {
                        in_digits = false;
                        out.push(ch);
                    }
                }
                out
            };
            (clean(&output.stdout), clean(&output.stderr))
        };
        [
            run(None, false),
            run(Some("blocking = \"fail_closed\"\n"), false),
            run(None, true),
            run(
                Some("blocking = \"fail_closed\"\nblock_timeout_secs = 1800\n"),
                true,
            ),
        ]
    })
    .await
    .expect("byte-identical fixture joins");

    // The determinate state the delay above buys: in BOTH scripted runs the
    // agent is still live when input ends, so the exit join is entered and its
    // two narration lines are compared rather than coin-flipped. If this ever
    // trips, the ordering has inverted and the byte comparison below has
    // quietly stopped covering the join path.
    for (index, (_, stderr)) in transcripts.iter().take(2).enumerate() {
        assert!(
            stderr.contains("input ended with N background agent(s) running; joining them")
                && stderr.contains("joined N background agent(s) before exit"),
            "scripted run {index} must reach end-of-input with the agent still joining: {stderr}"
        );
    }
    assert_eq!(
        transcripts[0], transcripts[1],
        "an explicit fail_closed must not change one byte of a scripted run"
    );
    assert_eq!(
        transcripts[2], transcripts[3],
        "nor one byte of a JSONL run (N1: background_result unchanged)"
    );
}

/// The end-to-end test this milestone has owed since T2-S2: `blocking = ask`
/// with a one-second `block_timeout_secs`. A detached agent hits a REAL
/// permission gate, the REAL prompt route parks it in the REAL registry,
/// `/jobs` shows it under `needs input`, and the REAL timer ends it as
/// failed/interrupted -- no `/jobs reply` required (that is T2-S5b).
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_blocking_ask_parks_a_gated_detached_agent_and_times_it_out() {
    let server = wiremock::MockServer::start().await;
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let outside = TempDir::new().expect("a directory no allow root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "secret").unwrap();
    mount_gated_detached_agent_gateway(&server, &forbidden).await;
    write_config_with_agents_table(
        home.path(),
        &server.uri(),
        "blocking = \"ask\"\nblock_timeout_secs = 1\n",
    );

    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let allow = repo_path.to_string_lossy().into_owned();
    let (status, transcript) = tokio::task::spawn_blocking(move || {
        run_gated_delegation_pty(&home_path, &repo_path, &["--allow-path", &allow])
    })
    .await
    .expect("interactive gated-delegation fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        !transcript.contains("unknown field"),
        "`blocking`/`block_timeout_secs` must be accepted keys: {transcript}"
    );
    assert!(
        transcript.contains("needs input"),
        "under `ask` the parked agent must appear in /jobs under `needs input`: {transcript}"
    );
    assert!(
        !transcript.contains("background agents cannot open interactive permission prompts"),
        "under `ask` the gate parks instead of returning the fail-closed error: {transcript}"
    );
    assert!(
        transcript.contains("interrupted"),
        "the configured one-second timeout must end the agent as failed/interrupted: {transcript}"
    );
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

/// The short spelling is only a CLI alias: both spellings take the same
/// existing non-prompting filesystem path, emit the same truthful warning and
/// machine policy, and do not disclose ambient secret-shaped values.
#[tokio::test]
#[serial]
async fn test_yolo_matches_dangerous_skip_permissions_without_a_prompt() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().expect("ungranted work dir");
    let target = work.path().join("created-without-a-prompt.txt");
    let tool_args = serde_json::json!({
        "path": target.to_string_lossy(),
        "content": "same existing authority path"
    })
    .to_string();
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
                        "id": "write_outside_roots",
                        "type": "function",
                        "function": {"name": "fs_write_file", "arguments": tool_args}
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
    let script = home.path().join("unattended.script");
    fs::write(&script, "create the requested file\n/permissions\n/quit\n").unwrap();

    let mut policy_receipts = Vec::new();
    for flag in ["--dangerously-skip-permissions", "--yolo"] {
        if target.exists() {
            fs::remove_file(&target).unwrap();
        }
        let home_path = home.path().to_path_buf();
        let script_path = script.clone();
        let output = tokio::task::spawn_blocking(move || {
            let mut cmd = base_cmd(&home_path);
            cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
                .env("DO_NOT_DISCLOSE_SECRET", "top-secret-fixture-value")
                .args([
                    "--gateway",
                    "example",
                    "--jsonl",
                    "--no-stream",
                    flag,
                    "chat",
                    "--no-session",
                    "--script",
                    &script_path.to_string_lossy(),
                ])
                .output()
                .expect("run unattended alias fixture")
        })
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "flag {flag}; stderr: {}; stdout: {}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "same existing authority path",
            "flag: {flag}"
        );
        let stderr = String::from_utf8(output.stderr).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stderr.contains("DANGEROUSLY SKIPPING PERMISSIONS"));
        assert!(!stderr.contains("permission —"), "flag {flag}: {stderr}");
        assert!(!stderr.contains("top-secret-fixture-value"));
        assert!(!stdout.contains("top-secret-fixture-value"));
        let events = stdout
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(events
            .iter()
            .all(|event| event["type"] != "permission_prompt"));
        let permissions = events
            .iter()
            .rfind(|event| event["result"]["kind"] == "permissions")
            .expect("permissions payload");
        policy_receipts.push(serde_json::json!({
            "mode": permissions["result"]["mode"],
            "dangerous_global_bypass": permissions["result"]["dangerous_global_bypass"],
            "command_mode": permissions["result"]["commands"]["mode"],
        }));
    }

    assert_eq!(policy_receipts[0], policy_receipts[1]);
    assert_eq!(
        policy_receipts[0]["mode"],
        "DANGER: global filesystem bypass"
    );
    assert_eq!(policy_receipts[0]["dangerous_global_bypass"], true);
    assert!(
        policy_receipts[0]["command_mode"].is_null(),
        "the bypass alias must not enable command capability by itself"
    );
    server.verify().await;
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

/// `/jobs view` and its `/agents` alias, end to end over the JSONL script
/// surface. The alias arm historically rejected any non-numeric argument, so
/// the `/agents view` assertion is the one that catches a silent regression.
/// A separate script, not an extension of the existing one, keeps every
/// `results[N]` index in `test_jsonl_script_query_results_are_structured_and_framed`
/// untouched.
#[test]
#[serial]
fn test_jsonl_script_jobs_view_is_served_by_both_the_command_and_its_alias() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("jobs-view.txt");
    fs::write(&script, "/jobs view\n/agents view\n/agents\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["--jsonl", "chat", "--script", &script.to_string_lossy()])
        .output()
        .expect("jobs view script");
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
    assert_eq!(results.len(), 3);

    // `/jobs view`: the grouped deck is a `jobs` action with the same snapshot
    // payload the flat `list` action already publishes.
    assert_eq!(results[0]["result"]["kind"], "jobs");
    assert_eq!(results[0]["result"]["action"], "view");
    assert_eq!(results[0]["result"]["changed"], false);
    assert_eq!(results[0]["result"]["snapshot"]["total"], 0);
    assert!(results[0]["result"]["snapshot"]["agents"].is_array());
    assert!(results[0]["result"]["alias"].is_null());

    // `/agents view`: the alias forwards a non-numeric argument verbatim
    // instead of erroring, and still stamps its own `alias` field.
    assert_eq!(results[1]["result"]["kind"], "jobs");
    assert_eq!(results[1]["result"]["action"], "view");
    assert_eq!(results[1]["result"]["alias"], "agents");
    assert_eq!(results[1]["result"]["snapshot"]["total"], 0);

    // Bare `/agents` still means `list`: widening the alias keeps its shapes.
    assert_eq!(results[2]["result"]["kind"], "jobs");
    assert_eq!(results[2]["result"]["action"], "list");
    assert_eq!(results[2]["result"]["alias"], "agents");
}

/// `/agents <id>` still maps to `show <id>`. With no background agents a
/// nonexistent id is the only reachable case and the handler rejects it, so
/// this runs without `--jsonl` to read the human message on stderr: only that
/// message distinguishes "forwarded to `show`" from "rejected by the alias".
#[test]
#[serial]
fn test_script_agents_with_an_id_still_maps_to_show() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("agents-id.txt");
    fs::write(&script, "/agents 7\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["chat", "--script", &script.to_string_lossy()])
        .output()
        .expect("agents id script");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    // The argument reached `/jobs show 7`, not the alias' own usage line.
    assert!(stderr.contains("no background job #7"), "{stderr}");
    assert!(
        !stderr.contains("usage: /agents"),
        "the alias rejected its own numeric form: {stderr}"
    );
}

/// `/jobs explain <id>` and `/agents explain <id>`. With no background agents
/// the unknown-id rejection is the only reachable outcome, and under `--jsonl`
/// both a forwarded `explain` and an alias-level rejection surface as the same
/// framed failure — so this runs without `--jsonl` and reads stderr, which is
/// the only place the two paths differ.
#[test]
#[serial]
fn test_script_jobs_explain_is_served_by_both_the_command_and_its_alias() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("jobs-explain.txt");
    fs::write(&script, "/jobs explain 7\n/agents explain 7\n/quit\n").unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["chat", "--script", &script.to_string_lossy()])
        .output()
        .expect("jobs explain script");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Both spellings reached the `explain` arm and were rejected by the id
    // lookup, not by the subcommand table or by the alias.
    assert_eq!(
        stderr.matches("no background job #7").count(),
        2,
        "{stderr}"
    );
    assert!(!stderr.contains("unknown action 'explain'"), "{stderr}");
    assert!(!stderr.contains("usage: /jobs ["), "{stderr}");
    assert!(!stderr.contains("usage: /agents"), "{stderr}");
    // A rejected lookup is not a usage error either.
    assert!(!stderr.contains("usage: /jobs explain"), "{stderr}");
}

/// Run a one-command script with `agents_toml` appended to the config and the
/// child pointed at its own runtime directory. Returns that runtime directory
/// so the caller can assert on what the run left behind. The parent's own
/// environment is never touched: `XDG_RUNTIME_DIR` is set on the child only.
fn run_script_with_runtime_dir(
    home: &TempDir,
    runtime: &TempDir,
    script_name: &str,
    agents_toml: &str,
) {
    write_config(home.path(), "http://127.0.0.1:1");
    if !agents_toml.is_empty() {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(home.path().join(".kitsu").join("config.toml"))
            .expect("open config.toml to append the [agents] table");
        write!(file, "\n{agents_toml}").expect("append the [agents] table");
    }
    let script = home.path().join(script_name);
    fs::write(&script, "/jobs view\n/quit\n").expect("write script");

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .env("XDG_RUNTIME_DIR", runtime.path())
        .args(["--jsonl", "chat", "--script", &script.to_string_lossy()])
        .output()
        .expect("scripted run");
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

/// The roadmap invariant: an optional integration left at its default writes
/// no state at all. The file itself is removed on exit, so a finished run
/// cannot be judged by the file — but the *directory* is never removed, so its
/// absence proves no write was ever attempted.
#[test]
#[serial]
fn test_script_run_with_default_config_writes_no_agent_state_directory() {
    let home = TempDir::new().expect("temp home");
    let runtime = TempDir::new().expect("temp runtime dir");

    run_script_with_runtime_dir(&home, &runtime, "state-default.txt", "");

    let dir = runtime.path().join("kitsu");
    assert!(
        !dir.exists(),
        "a scripted carrier under the default `auto` mode must create nothing, found {dir:?}"
    );
    assert_eq!(
        fs::read_dir(runtime.path()).unwrap().count(),
        0,
        "the runtime directory must be untouched"
    );
}

/// `state_file = "on"` overrides the carrier rule: a scripted run writes. The
/// per-pid file is gone because exit removed it, and the owner-only directory
/// it left behind is the evidence that the write happened.
#[test]
#[serial]
fn test_script_run_with_agents_state_file_on_writes_the_state_directory() {
    let home = TempDir::new().expect("temp home");
    let runtime = TempDir::new().expect("temp runtime dir");

    run_script_with_runtime_dir(
        &home,
        &runtime,
        "state-on.txt",
        "[agents]\nstate_file = \"on\"\n",
    );

    let dir = runtime.path().join("kitsu");
    assert!(
        dir.is_dir(),
        "`state_file = \"on\"` must create the state directory at {dir:?}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "the state directory must be owner-only"
        );
    }

    let leftovers = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "exit must remove the per-pid file and leave no temporary: {leftovers:?}"
    );
}

/// The explicit opt-out works independently of the carrier rule: `off` is
/// indistinguishable from the default here, which is the point.
#[test]
#[serial]
fn test_script_run_with_agents_state_file_off_writes_no_agent_state_directory() {
    let home = TempDir::new().expect("temp home");
    let runtime = TempDir::new().expect("temp runtime dir");

    run_script_with_runtime_dir(
        &home,
        &runtime,
        "state-off.txt",
        "[agents]\nstate_file = \"off\"\n",
    );

    let dir = runtime.path().join("kitsu");
    assert!(
        !dir.exists(),
        "`state_file = \"off\"` must create nothing, found {dir:?}"
    );
    assert_eq!(fs::read_dir(runtime.path()).unwrap().count(), 0);
}

// -------------------------------------------------------------------------
// `kitsu agents`: the host-only reader for the advisory per-process state
// files (requirement H1). Each test points the child at its own
// XDG_RUNTIME_DIR; the parent's environment is never touched.
// -------------------------------------------------------------------------

/// A well-formed `kitsu.agents.v1` state file for `pid`, written by hand into
/// `$XDG_RUNTIME_DIR/kitsu` so the bytes under test are foreign to the writer.
fn write_agent_state_fixture(runtime: &Path, pid: u32, schema: &str, state: &str) {
    let dir = runtime.join("kitsu");
    fs::create_dir_all(&dir).expect("create the agent state directory");
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "schema": schema,
        "pid": pid,
        "cwd": "/workspace/kitsu",
        "session_id": "sess-7",
        "state": state,
        "counts": {
            "running": 1,
            "blocked": 2,
            "unacknowledged": 3,
            "terminal": 4,
            "failed": 5,
        },
        "threads": [],
        "updated_at": "2026-10-13T12:00:00Z",
    }))
    .unwrap();
    fs::write(dir.join(format!("{pid}.json")), body).expect("write the state fixture");
}

/// `kitsu agents --json` follows `kitsu sessions --json`: one pretty-printed
/// JSON value on stdout, here the bare array of entries.
#[test]
fn test_agents_json_lists_a_live_process_from_its_state_file() {
    let home = TempDir::new().expect("temp home");
    let runtime = TempDir::new().expect("temp runtime dir");
    write_config(home.path(), "http://127.0.0.1:1");
    // The test process is unquestionably alive, so liveness cannot make this
    // flaky and the entry must survive the default (live-only) filter.
    let pid = std::process::id();
    write_agent_state_fixture(runtime.path(), pid, "kitsu.agents.v1", "blocked");

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("XDG_RUNTIME_DIR", runtime.path())
        .args(["agents", "--json"])
        .output()
        .expect("kitsu agents --json");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("one JSON value");
    let entries = listed.as_array().expect("a bare JSON array of entries");
    assert_eq!(entries.len(), 1, "{listed}");
    let entry = &entries[0];
    assert_eq!(entry["pid"], pid);
    assert_eq!(entry["alive"], true);
    assert_eq!(entry["status"], "ok");
    assert_eq!(entry["schema"], "kitsu.agents.v1");
    assert_eq!(entry["state"], "blocked");
    assert_eq!(entry["cwd"], "/workspace/kitsu");
    assert_eq!(entry["session_id"], "sess-7");
    assert_eq!(entry["counts"]["running"], 1);
    assert_eq!(entry["counts"]["blocked"], 2);
    assert_eq!(entry["counts"]["unacknowledged"], 3);
    assert_eq!(entry["updated_at"], "2026-10-13T12:00:00Z");
    assert_eq!(entry["threads"], serde_json::json!([]));
    assert_eq!(entry["unsupported_threads"], 0);

    // A read reclaims nothing.
    assert!(runtime
        .path()
        .join("kitsu")
        .join(format!("{pid}.json"))
        .is_file());
}

/// The human tier: a deck row for the live process, and a plain sentence when
/// nothing is publishing - which is an empty answer, not a failure.
#[test]
fn test_agents_human_listing_prints_rows_and_an_empty_answer_exits_zero() {
    let home = TempDir::new().expect("temp home");
    let runtime = TempDir::new().expect("temp runtime dir");
    write_config(home.path(), "http://127.0.0.1:1");
    let pid = std::process::id();
    write_agent_state_fixture(runtime.path(), pid, "kitsu.agents.v1", "working");

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("XDG_RUNTIME_DIR", runtime.path())
        .arg("agents")
        .output()
        .expect("kitsu agents");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(stdout.contains("kitsu agents"), "{stdout}");
    assert!(stdout.contains("1 process(es)"), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "pid {pid} | working | running 1 | blocked 2 | unacknowledged 3 | /workspace/kitsu"
        )),
        "{stdout}"
    );

    let empty_runtime = TempDir::new().expect("temp runtime dir");
    let mut empty = base_cmd(home.path());
    let output = empty
        .env("XDG_RUNTIME_DIR", empty_runtime.path())
        .arg("agents")
        .output()
        .expect("kitsu agents against an empty runtime dir");
    assert!(
        output.status.success(),
        "an empty answer must exit zero, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("no kitsu process is publishing agent state"),
        "{stdout}"
    );
    assert!(
        !empty_runtime.path().join("kitsu").exists(),
        "the reader must not create the state directory"
    );
}

/// A newer sibling kitsu publishing a schema this binary does not know must
/// be named in the listing, never silently dropped and never fatal.
#[test]
fn test_agents_json_reports_a_newer_schema_as_unsupported_without_failing() {
    let home = TempDir::new().expect("temp home");
    let runtime = TempDir::new().expect("temp runtime dir");
    write_config(home.path(), "http://127.0.0.1:1");
    let pid = std::process::id();
    write_agent_state_fixture(runtime.path(), pid, "kitsu.agents.v2", "whatever-v2-says");

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("XDG_RUNTIME_DIR", runtime.path())
        .args(["agents", "--json"])
        .output()
        .expect("kitsu agents --json with a v2 file");
    assert!(
        output.status.success(),
        "a newer sibling must not break the listing, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("one JSON value");
    let entries = listed.as_array().expect("a bare JSON array of entries");
    assert_eq!(entries.len(), 1, "the entry must not be dropped: {listed}");
    assert_eq!(entries[0]["pid"], pid);
    assert_eq!(entries[0]["status"], "schema_unsupported");
    assert_eq!(
        entries[0]["schema"], "kitsu.agents.v2",
        "the schema found must be reported verbatim"
    );
}

/// The global `--jsonl` flag is reserved for chat automation events, and six
/// management paths already refuse it through `reject_jsonl_for_management`.
/// `agents` is the seventh: without this test the call can simply be omitted
/// and one management command would quietly accept a flag the others reject.
///
/// Once `--jsonl` is set that mode owns both channels: stdout carries only
/// `kitsu.chat.v1` records and the terminal error is framed as an `error`
/// event there, so human stderr is empty. The assertion shape therefore
/// follows `test_jsonl_preflight_error_keeps_stdout_machine_only` - the same
/// inline line-by-line parse plus `jsonl_fixture::inspect_jsonl` - rather than
/// the human `--output-last-message` rejection.
#[test]
fn test_agents_rejects_the_global_jsonl_flag_and_names_its_replacement() {
    let home = TempDir::new().expect("temp home");
    let runtime = TempDir::new().expect("temp runtime dir");
    write_config(home.path(), "http://127.0.0.1:1");
    // A populated runtime directory, so the only possible cause of failure is
    // the flag check and not an empty listing.
    write_agent_state_fixture(
        runtime.path(),
        std::process::id(),
        "kitsu.agents.v1",
        "idle",
    );

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("XDG_RUNTIME_DIR", runtime.path())
        .args(["--jsonl", "agents", "--json"])
        .output()
        .expect("kitsu --jsonl agents --json");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "a JSONL run frames its terminal error on stdout and leaves stderr empty"
    );

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
    assert_eq!(events[0]["kind"], "user");
    assert_eq!(events[0]["exit_code"], 1);
    let message = events[0]["message"].as_str().expect("an error message");
    assert!(message.contains("--jsonl"), "{message}");
    assert!(
        message.contains("kitsu agents --json"),
        "the error must name the replacement for this command: {message}"
    );
    assert_eq!(events[1]["user_failures"], 1);

    let facts = jsonl_fixture::inspect_jsonl(&events, 1).expect("inspect the rejection JSONL");
    assert_eq!(facts.class, jsonl_fixture::JsonlOutcomeClass::UserFault);
}

// --- T2-S7 end to end: the blocked state as an outside tool sees it -------
//
// The only test in the tree that observes a blocked agent from OUTSIDE the
// process that owns it: a real PTY carrier parks a real detached agent on a
// real permission gate, and while it is parked this test reads the state file
// the carrier wrote to disk and runs `kitsu agents --json` as a SEPARATE
// process against it. The unit tests beside the writer pin the content; this
// one pins that the bytes actually travel, and that the two machine surfaces
// spell the same thing.

/// The one `<pid>.json` state file under `runtime/kitsu`, decoded.
///
/// `None` while the carrier has not published yet, which is what lets the
/// driver synchronize on the file rather than sleep for it.
#[cfg(unix)]
fn read_only_agent_state_file(runtime: &Path) -> Option<(u32, serde_json::Value)> {
    let dir = runtime.join("kitsu");
    let mut found = None;
    for item in fs::read_dir(&dir).ok()?.flatten() {
        let name = item.file_name();
        let name = name.to_str()?;
        let Some(digits) = name.strip_suffix(".json") else {
            continue;
        };
        let Ok(pid) = digits.parse::<u32>() else {
            continue;
        };
        let bytes = fs::read(item.path()).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        assert!(
            found.is_none(),
            "exactly one carrier publishes into a private runtime dir, found a second: {name}"
        );
        found = Some((pid, value));
    }
    found
}

/// Plan row T2-S7's acceptance over the whole real stack. `blocking = "ask"`
/// with a 600s timeout, so only the typed answer can release the agent, and
/// `state_file = "on"` so the hint is published whatever the carrier is
/// inferred to be.
///
/// Why this is not vacuous, and what would break it: every assertion is a
/// positive one about a surface outside the process. Reverting
/// `AgentApprovalKind::wire()` to the empty string, or `AgentThreadState`'s
/// typed `waiting_for` back to an unconditional `None`, fails step 2 and 3
/// immediately; leaving `waiting_for` set after `unblock()` fails step 5. The
/// leak sweeps are the only negative assertions, and they are backed by the
/// positive `read_file` equality in the same test -- an empty or absent record
/// cannot pass them by carrying nothing at all -- plus the probe assertions in
/// the unit tests proving both surfaces print an argument when given one.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_a_parked_agent_is_visible_in_the_state_file_and_to_kitsu_agents_json() {
    let server = wiremock::MockServer::start().await;
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let runtime = TempDir::new().expect("temp runtime dir");
    let outside = TempDir::new().expect("a directory no allow root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "GATED_BYTES").unwrap();
    mount_gated_detached_agent_gateway(&server, &forbidden).await;
    // `state_file = "on"`, spelled as README's `[agents] state_file` and
    // `config.rs` accept it: `on` publishes regardless of the inferred
    // carrier, so this test does not depend on carrier inference at all.
    write_config_with_agents_table(
        home.path(),
        &server.uri(),
        "blocking = \"ask\"\nblock_timeout_secs = 600\nstate_file = \"on\"\n",
    );

    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let runtime_path = runtime.path().to_path_buf();
    let allow = repo_path.to_string_lossy().into_owned();
    // The tokens a leak would carry. The gated request is a READ of
    // `secret.txt` outside every allow root, so these are this fixture's real
    // arguments rather than invented ones. None is a bare common word: `rm`
    // alone would also forbid "permission", "format" and "confirm" and would
    // constrain the product by accident (F41).
    let forbidden_path = forbidden.to_string_lossy().into_owned();
    let outside_dir = outside.path().to_string_lossy().into_owned();

    let (status, transcript, observed) = tokio::task::spawn_blocking({
        let (home_path, repo_path, runtime_path) =
            (home_path.clone(), repo_path.clone(), runtime_path.clone());
        move || {
            // What the driver captured while the agent was parked, and once
            // more after it was released.
            let mut blocked_state: Option<serde_json::Value> = None;
            let mut blocked_listing: Option<serde_json::Value> = None;
            let mut released_state: Option<serde_json::Value> = None;

            let mut delegated = false;
            let mut replied = false;
            let mut quit = false;
            let mut polls = 0_u32;
            let mut last_poll = std::time::Instant::now();
            let driver_runtime = runtime_path.clone();
            let driver_home = home_path.clone();
            let (status, transcript) = run_plain_interactive_chat_pty_with_env(
                &home_path,
                &repo_path,
                &["--allow-path", &allow],
                &[("XDG_RUNTIME_DIR", runtime_path.as_path())],
                |output, terminal| {
                    use std::io::Write as _;
                    let text = String::from_utf8_lossy(output);
                    if !delegated && text.matches("kitsu>").count() >= 1 {
                        terminal
                            .write_all(b"delegate the read\n")
                            .expect("delegate");
                        terminal.flush().unwrap();
                        delegated = true;
                        last_poll = std::time::Instant::now();
                        return;
                    }
                    if !delegated || quit || !text.contains("PARENT_DELEGATED") {
                        return;
                    }
                    if !replied {
                        // `/jobs view` -- NOT bare `/jobs`: only
                        // `format_jobs_view` emits the `needs input` heading.
                        let Some(id) = jobs_view_needs_input_id(&text) else {
                            if last_poll.elapsed() >= std::time::Duration::from_millis(150) {
                                terminal.write_all(b"/jobs view\n").expect("poll");
                                terminal.flush().unwrap();
                                polls += 1;
                                last_poll = std::time::Instant::now();
                            }
                            if polls >= 40 {
                                terminal
                                    .write_all(b"/jobs stop-all\n/quit\n")
                                    .expect("quit");
                                terminal.flush().unwrap();
                                quit = true;
                            }
                            return;
                        };
                        // The agent is parked AND the carrier has been round
                        // the interactive loop at least once since, so the
                        // state file it republishes each iteration is current.
                        let Some((pid, state)) = read_only_agent_state_file(&driver_runtime) else {
                            if polls >= 40 {
                                terminal
                                    .write_all(b"/jobs stop-all\n/quit\n")
                                    .expect("quit");
                                terminal.flush().unwrap();
                                quit = true;
                            }
                            if last_poll.elapsed() >= std::time::Duration::from_millis(150) {
                                terminal.write_all(b"/jobs view\n").expect("poll");
                                terminal.flush().unwrap();
                                polls += 1;
                                last_poll = std::time::Instant::now();
                            }
                            return;
                        };
                        if state["counts"]["blocked"] != 1 {
                            // Published before the park landed; poll again.
                            if last_poll.elapsed() >= std::time::Duration::from_millis(150) {
                                terminal.write_all(b"/jobs view\n").expect("poll");
                                terminal.flush().unwrap();
                                polls += 1;
                                last_poll = std::time::Instant::now();
                            }
                            if polls >= 40 {
                                terminal
                                    .write_all(b"/jobs stop-all\n/quit\n")
                                    .expect("quit");
                                terminal.flush().unwrap();
                                quit = true;
                            }
                            return;
                        }
                        // Surface 3, from a SEPARATE process, against the live
                        // file this carrier is still holding open.
                        let mut cmd = base_cmd(&driver_home);
                        let listing = cmd
                            .env("XDG_RUNTIME_DIR", driver_runtime.as_path())
                            .args(["agents", "--json"])
                            .output()
                            .expect("kitsu agents --json against a live carrier");
                        assert!(
                            listing.status.success(),
                            "kitsu agents --json failed: {}",
                            String::from_utf8_lossy(&listing.stderr)
                        );
                        let listed: serde_json::Value =
                            serde_json::from_slice(&listing.stdout).expect("one JSON value");
                        assert_eq!(
                            listed[0]["pid"], pid,
                            "the listing must describe the carrier that wrote the file: {listed}"
                        );
                        blocked_state = Some(state);
                        blocked_listing = Some(listed);

                        terminal
                            .write_all(format!("/jobs reply {id} y\n").as_bytes())
                            .expect("reply");
                        terminal.flush().unwrap();
                        replied = true;
                        polls = 0;
                        last_poll = std::time::Instant::now();
                        return;
                    }
                    let settled = text.contains("CHILD_SAW") || text.contains("interrupted");
                    if settled || polls >= 40 {
                        // `stop-all` BEFORE `/quit`: a still-parked agent would
                        // make `/quit` hit the exit guard and the run would die
                        // on the harness deadline instead of its own assertion.
                        released_state =
                            read_only_agent_state_file(&driver_runtime).map(|(_, value)| value);
                        terminal
                            .write_all(b"/jobs stop-all\n/quit\n")
                            .expect("quit");
                        terminal.flush().unwrap();
                        quit = true;
                        return;
                    }
                    if last_poll.elapsed() >= std::time::Duration::from_millis(150) {
                        // `/agents` too, from here on: only the deck prints a
                        // finished agent's result, so `CHILD_SAW` -- the
                        // settle signal -- can reach the transcript at all.
                        terminal.write_all(b"/jobs view\n/agents\n").expect("poll");
                        terminal.flush().unwrap();
                        polls += 1;
                        last_poll = std::time::Instant::now();
                    }
                },
            );
            (
                status,
                transcript,
                (blocked_state, blocked_listing, released_state),
            )
        }
    })
    .await
    .expect("the interactive state-file fixture joins");

    let (blocked_state, blocked_listing, released_state) = observed;
    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        transcript.contains("needs input"),
        "the agent must park first: {transcript}"
    );
    assert!(
        transcript.contains("CHILD_SAW"),
        "the typed answer must release the agent: {transcript}"
    );
    assert!(
        !transcript.contains("interrupted"),
        "a 600s block timeout cannot be what released it: {transcript}"
    );

    // 2. The state file on disk, written by a real process while parked.
    let state = blocked_state.unwrap_or_else(|| {
        panic!("no state file was observed while the agent was parked: {transcript}")
    });
    assert_eq!(state["schema"], "kitsu.agents.v1", "{state}");
    assert_eq!(
        state["state"], "blocked",
        "a process holding a parked agent publishes `blocked`: {state}"
    );
    assert_eq!(state["counts"]["blocked"], 1, "{state}");
    let threads = state["threads"].as_array().expect("an array of threads");
    assert_eq!(threads.len(), 1, "{state}");
    assert_eq!(threads[0]["thread"]["phase"], "blocked", "{state}");
    assert_eq!(
        threads[0]["thread"]["waiting_for"], "read_file",
        "the gated request is a READ, so the published kind is read_file: {state}"
    );

    // 3. The same thing seen by a separate process through `kitsu agents`.
    let listing = blocked_listing.expect("the listing is captured with the state file");
    let entries = listing.as_array().expect("a bare JSON array of entries");
    assert_eq!(entries.len(), 1, "{listing}");
    assert_eq!(entries[0]["status"], "ok", "{listing}");
    assert_eq!(entries[0]["alive"], true, "{listing}");
    assert_eq!(
        entries[0]["state"], "blocked",
        "`kitsu agents --json` must report the blocked process: {listing}"
    );
    assert_eq!(entries[0]["counts"]["blocked"], 1, "{listing}");
    assert_eq!(
        entries[0]["unsupported_threads"], 0,
        "the carrier's own thread record must be understood: {listing}"
    );
    let listed_threads = entries[0]["threads"]
        .as_array()
        .expect("an array of threads");
    assert_eq!(listed_threads.len(), 1, "{listing}");
    assert_eq!(listed_threads[0]["thread"]["phase"], "blocked", "{listing}");
    // The point of the whole test: the two machine surfaces agree byte for
    // byte about what the agent waits on.
    assert_eq!(
        listed_threads[0]["thread"]["waiting_for"], "read_file",
        "`kitsu agents --json` must report what the agent waits for: {listing}"
    );
    assert_eq!(
        listed_threads[0]["thread"]["waiting_for"], threads[0]["thread"]["waiting_for"],
        "the state file and `kitsu agents --json` must not disagree"
    );

    // 4. The leak sweep, on both outputs.
    let state_bytes = serde_json::to_string(&state).unwrap();
    let listing_bytes = serde_json::to_string(&listing).unwrap();
    for token in [
        forbidden_path.as_str(),
        outside_dir.as_str(),
        "secret.txt",
        "GATED_BYTES",
    ] {
        assert!(
            !state_bytes.contains(token),
            "the state file leaked the parked request's argument ({token}): {state_bytes}"
        );
        assert!(
            !listing_bytes.contains(token),
            "`kitsu agents --json` leaked the parked request's argument ({token}): {listing_bytes}"
        );
    }
    // The transcript legitimately carries the bytes (the child's own answer
    // does), which is exactly why the two surfaces above having none of them
    // is a statement about those surfaces and not about the fixture.
    assert!(
        transcript.contains("GATED_BYTES"),
        "`y` is AllowOnce, so the gated read succeeds and the fixture really \
         did handle these bytes: {transcript}"
    );

    // 5. Released: no stale reason left behind for a poller.
    let released = released_state
        .unwrap_or_else(|| panic!("no state file was observed after the answer: {transcript}"));
    assert_eq!(
        released["counts"]["blocked"], 0,
        "the answered agent is no longer counted blocked: {released}"
    );
    assert_ne!(
        released["state"], "blocked",
        "the process is no longer blocked: {released}"
    );
    for dto in released["threads"].as_array().expect("threads") {
        assert!(
            dto["thread"].get("waiting_for").is_none(),
            "a released agent must leave no waiting_for key at all: {released}"
        );
    }
}

// --- T2-S7: `kitsu agents --json` reports the blocked state ---------------
//
// The third surface of T2-S7. Unlike the other two it is a pure *reader*: the
// bytes it prints are the state file surface 1 writes, so the content of
// `waiting_for` is pinned in the unit tests beside the writer and what is
// pinned here is the pass-through and the refusal.

/// A `kitsu.agents.v1` state file whose single thread record is `thread`,
/// written by hand so the bytes under test are foreign to the writer.
fn write_agent_state_fixture_with_thread(
    runtime: &Path,
    pid: u32,
    state: &str,
    thread: serde_json::Value,
) {
    let dir = runtime.join("kitsu");
    fs::create_dir_all(&dir).expect("create the agent state directory");
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "schema": "kitsu.agents.v1",
        "pid": pid,
        "cwd": "/workspace/kitsu",
        "session_id": "sess-7",
        "state": state,
        "counts": {
            "running": 0,
            "blocked": 1,
            "unacknowledged": 0,
            "terminal": 0,
            "failed": 0,
        },
        "threads": [thread],
        "updated_at": "2026-10-14T12:00:00Z",
    }))
    .unwrap();
    fs::write(dir.join(format!("{pid}.json")), body).expect("write the state fixture");
}

/// A valid non-terminal `blocked` thread record whose `waiting_for` is
/// `waiting_for`. An empty, open timeline keeps the DTO's accounting trivially
/// consistent, so only the field under test can decide the outcome.
fn blocked_thread_fixture(waiting_for: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 2,
        "thread": {
            "id": 1,
            "task": "summarize the crate",
            "model": "qwen2.5-coder",
            "phase": "blocked",
            "waiting_for": waiting_for,
            "handoff": "unavailable",
            "elapsed_ms": 12_500,
            "steering_open": true,
            "pending_steering": 0,
            "timeline": {
                "entries": [],
                "dropped_entries": 0,
                "retained_bytes": 0,
                "closed": false,
            },
        },
    })
}

/// `kitsu agents --json` reports the blocked state and what the agent waits
/// for, passing the permission kind through unchanged.
#[test]
fn test_agents_json_reports_the_blocked_state_and_the_kind_it_waits_for() {
    let home = TempDir::new().expect("temp home");
    let runtime = TempDir::new().expect("temp runtime dir");
    write_config(home.path(), "http://127.0.0.1:1");
    let pid = std::process::id();
    write_agent_state_fixture_with_thread(
        runtime.path(),
        pid,
        "blocked",
        blocked_thread_fixture(serde_json::json!("write_file")),
    );

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("XDG_RUNTIME_DIR", runtime.path())
        .args(["agents", "--json"])
        .output()
        .expect("kitsu agents --json");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).expect("one JSON value");
    let entries = listed.as_array().expect("a bare JSON array of entries");
    assert_eq!(entries.len(), 1, "{listed}");
    let entry = &entries[0];
    assert_eq!(entry["status"], "ok");
    assert_eq!(entry["state"], "blocked");
    assert_eq!(
        entry["unsupported_threads"], 0,
        "a blocked thread record must be understood, not counted as unsupported: {entry}"
    );
    let threads = entry["threads"].as_array().expect("an array of threads");
    assert_eq!(threads.len(), 1, "{entry}");
    assert_eq!(threads[0]["thread"]["phase"], "blocked", "{entry}");
    assert_eq!(
        threads[0]["thread"]["waiting_for"], "write_file",
        "the permission kind reaches the reader unchanged: {entry}"
    );
}

/// The leak test for the reader. A foreign writer -- a newer kitsu, a
/// hand-edited file, a future field misuse -- that put a path or an argv
/// string in `waiting_for` must not have it printed: the field is a closed
/// unit enum, so such a record is refused as unsupported and costs only
/// itself.
///
/// Not vacuous: the preceding test proves this exact slot, in this exact
/// fixture, is accepted and printed when it holds a real kind. The refusals
/// below are therefore about the value, not about the fixture. Each assertion
/// also pins what the output *is* (`unsupported_threads == 1`, no threads)
/// rather than relying on the substring sweep alone, and the swept tokens are
/// full paths and `rm -rf`, not the bare word `rm` (F41).
#[test]
fn test_agents_json_refuses_a_waiting_for_that_carries_a_path_or_an_argv_word() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let pid = std::process::id();

    for (leak, tokens) in [
        (
            serde_json::json!("/work/notes.txt"),
            &["/work/notes.txt", "notes.txt"][..],
        ),
        (
            serde_json::json!("rm -rf /work/tmp"),
            &["rm -rf", "/work/tmp"][..],
        ),
        (
            serde_json::json!({"write_file": "/work/notes.txt"}),
            &["/work/notes.txt"][..],
        ),
    ] {
        let runtime = TempDir::new().expect("temp runtime dir");
        write_agent_state_fixture_with_thread(
            runtime.path(),
            pid,
            "blocked",
            blocked_thread_fixture(leak.clone()),
        );

        let mut cmd = base_cmd(home.path());
        let output = cmd
            .env("XDG_RUNTIME_DIR", runtime.path())
            .args(["agents", "--json"])
            .output()
            .expect("kitsu agents --json");
        assert!(
            output.status.success(),
            "an unusable thread record must not fail the listing ({leak}), stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let listed: serde_json::Value = serde_json::from_str(&stdout).expect("one JSON value");
        let entries = listed.as_array().expect("a bare JSON array of entries");
        assert_eq!(entries.len(), 1, "the process row survives: {listed}");
        assert_eq!(
            entries[0]["threads"],
            serde_json::json!([]),
            "{leak} must not be printed as a thread record: {listed}"
        );
        assert_eq!(
            entries[0]["unsupported_threads"], 1,
            "the record costs itself and nothing else: {listed}"
        );
        for token in tokens {
            assert!(
                !stdout.contains(token),
                "`kitsu agents --json` printed a parked argument ({token}) for {leak}: {stdout}"
            );
        }
    }
}

// --- T2-S5b: `/jobs reply <id> <y|a|n>` ------------------------------------
//
// FAILING-FIRST. No dispatcher has a `reply` arm yet, so the scripted runs
// below currently print the did-you-mean nudge and the usage line instead of
// reaching an id lookup.

/// The idle dispatcher's wording, read from stderr because that is the only
/// place the three outcomes and the usage line differ. With no background
/// agents the unknown-id rejection is the only reachable outcome, and the
/// `/agents` alias must reach the same arm (T1-S3 forwards verbatim).
#[test]
#[serial]
fn test_script_jobs_reply_is_served_by_both_the_command_and_its_alias() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("jobs-reply.txt");
    fs::write(
        &script,
        "/jobs reply 7 y\n/agents reply 7 y\n/agent reply 7 y\n/quit\n",
    )
    .unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["chat", "--script", &script.to_string_lossy()])
        .output()
        .expect("jobs reply script");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // All three spellings reached the `reply` arm and were rejected by the id
    // lookup, not by the subcommand table or by the alias.
    assert_eq!(
        stderr.matches("no background job #7").count(),
        3,
        "{stderr}"
    );
    assert!(!stderr.contains("unknown action 'reply'"), "{stderr}");
    assert!(!stderr.contains("usage: /jobs ["), "{stderr}");
    assert!(!stderr.contains("usage: /jobs reply"), "{stderr}");
}

/// Bad input is a usage error on the idle path too, and the usage line names
/// the three keys. Site 7: the unknown-subcommand line must list `reply`.
#[test]
#[serial]
fn test_script_jobs_reply_rejects_bad_input_with_a_usage_line() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let script = home.path().join("jobs-reply-bad.txt");
    fs::write(
        &script,
        "/jobs reply\n/jobs reply notanumber y\n/jobs reply 7\n/jobs reply 7 maybe\n/jobs nosuchthing\n/quit\n",
    )
    .unwrap();

    let mut cmd = base_cmd(home.path());
    let output = cmd
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["chat", "--script", &script.to_string_lossy()])
        .output()
        .expect("jobs reply bad-input script");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert_eq!(
        stderr.matches("usage: /jobs reply <id> <y|a|n>").count(),
        4,
        "each of the four bad forms prints the reply usage line: {stderr}"
    );
    // A bad key is never silently treated as a denial.
    assert!(!stderr.contains("no background job #7"), "{stderr}");
    // Site 7: the unknown-subcommand usage line advertises reply.
    assert!(stderr.contains("unknown action 'nosuchthing'"), "{stderr}");
    assert!(
        stderr.contains("reply <id> <y|a|n>]"),
        "the /jobs usage line must advertise reply: {stderr}"
    );
}

/// Drive the interactive PTY the way `run_gated_delegation_pty` does, but
/// answer the parked agent instead of waiting for the timer: poll `/jobs view`
/// until `needs input` appears, send `/jobs reply <id> <key>`, then keep
/// polling `/agents` until the released child's answer is printed.
///
/// `/jobs view` -- NOT bare `/jobs`. The `needs input` heading is produced only
/// by `format_jobs_view`; the bare command prints the agent deck, which never
/// contains that string. Polling the deck would make the positive assertion
/// unreachable at any timeout and the negative guards pass vacuously.
#[cfg(unix)]
fn run_gated_delegation_pty_answering(
    home: &Path,
    cwd: &Path,
    extra_chat_args: &[&str],
    key: &'static str,
) -> (std::process::ExitStatus, String) {
    use std::io::Write as _;

    let mut delegated = false;
    let mut replied = false;
    let mut quit = false;
    let mut polls = 0_u32;
    let mut last_poll = std::time::Instant::now();
    run_plain_interactive_chat_pty(home, cwd, extra_chat_args, move |output, terminal| {
        let text = String::from_utf8_lossy(output);
        let prompts = text.matches("kitsu>").count();
        if !delegated && prompts >= 1 {
            terminal
                .write_all(b"delegate the read\n")
                .expect("delegate");
            terminal.flush().unwrap();
            delegated = true;
            last_poll = std::time::Instant::now();
            return;
        }
        if !delegated || quit {
            return;
        }
        if !text.contains("PARENT_DELEGATED") {
            return;
        }
        if !replied {
            // Synchronize on the heading only `format_jobs_view` emits, then
            // answer the id the `needs input` group names.
            if let Some(id) = jobs_view_needs_input_id(&text) {
                terminal
                    .write_all(format!("/jobs reply {id} {key}\n").as_bytes())
                    .expect("reply");
                terminal.flush().unwrap();
                replied = true;
                last_poll = std::time::Instant::now();
                return;
            }
        }
        let settled = text.contains("CHILD_SAW") || text.contains("interrupted");
        if settled || polls >= 40 {
            // `stop-all` first: the block timeout is deliberately longer than
            // this test, so a still-parked agent would otherwise make `/quit`
            // hang on the exit guard and the run would die on the harness
            // deadline instead of on this test's own assertion.
            terminal
                .write_all(b"/jobs stop-all\n/quit\n")
                .expect("quit");
            terminal.flush().unwrap();
            quit = true;
            return;
        }
        if last_poll.elapsed() >= std::time::Duration::from_millis(150) {
            terminal.write_all(b"/jobs view\n/agents\n").expect("poll");
            terminal.flush().unwrap();
            polls += 1;
            last_poll = std::time::Instant::now();
        }
    })
}

/// The first `#<digits>` printed under the `needs input` heading of a
/// `/jobs view` render. `None` until that heading has actually been emitted,
/// which is what makes the driver above synchronize rather than sleep.
#[cfg(unix)]
fn jobs_view_needs_input_id(transcript: &str) -> Option<u64> {
    let after = transcript.rsplit_once("needs input")?.1;
    let digits = after.split_once('#')?.1;
    let id: String = digits.chars().take_while(char::is_ascii_digit).collect();
    id.parse().ok()
}

/// Plan row T2-S5's acceptance over the whole real stack: `blocking = "ask"`
/// with a block timeout far longer than the test, so the ONLY thing that can
/// release the parked agent is the typed answer. `y` lets the gated read
/// through, so the child's own answer carries the file's bytes back.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jobs_reply_allow_releases_a_parked_detached_agent() {
    let server = wiremock::MockServer::start().await;
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let outside = TempDir::new().expect("a directory no allow root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "GATED_BYTES").unwrap();
    mount_gated_detached_agent_gateway(&server, &forbidden).await;
    write_config_with_agents_table(
        home.path(),
        &server.uri(),
        "blocking = \"ask\"\nblock_timeout_secs = 600\n",
    );

    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let allow = repo_path.to_string_lossy().into_owned();
    let (status, transcript) = tokio::task::spawn_blocking(move || {
        run_gated_delegation_pty_answering(&home_path, &repo_path, &["--allow-path", &allow], "y")
    })
    .await
    .expect("interactive gated-delegation fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        transcript.contains("needs input"),
        "the agent must park first: {transcript}"
    );
    assert!(
        transcript.contains("CHILD_SAW"),
        "the typed answer must release the agent and let its turn finish: {transcript}"
    );
    assert!(
        transcript.contains("GATED_BYTES"),
        "`y` is AllowOnce, so the gated read succeeds: {transcript}"
    );
    assert!(
        !transcript.contains("interrupted"),
        "a 600s block timeout cannot be what released it: {transcript}"
    );
}

/// `n` is an answer too: the gate turns it into a policy denial and the agent
/// resumes with that error rather than being interrupted.
#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_jobs_reply_deny_releases_a_parked_detached_agent_with_a_denial() {
    let server = wiremock::MockServer::start().await;
    let home = TempDir::new().expect("temp home");
    let repo = TempDir::new().expect("temp repo");
    let outside = TempDir::new().expect("a directory no allow root covers");
    let forbidden = outside.path().join("secret.txt");
    fs::write(&forbidden, "GATED_BYTES").unwrap();
    mount_gated_detached_agent_gateway(&server, &forbidden).await;
    write_config_with_agents_table(
        home.path(),
        &server.uri(),
        "blocking = \"ask\"\nblock_timeout_secs = 600\n",
    );

    let home_path = home.path().to_path_buf();
    let repo_path = repo.path().to_path_buf();
    let allow = repo_path.to_string_lossy().into_owned();
    let (status, transcript) = tokio::task::spawn_blocking(move || {
        run_gated_delegation_pty_answering(&home_path, &repo_path, &["--allow-path", &allow], "n")
    })
    .await
    .expect("interactive gated-delegation fixture joins");

    assert!(status.success(), "status={status}; transcript={transcript}");
    assert!(
        transcript.contains("needs input"),
        "the agent must park first: {transcript}"
    );
    assert!(
        transcript.contains("CHILD_SAW"),
        "a denial releases the agent rather than interrupting it: {transcript}"
    );
    assert!(
        !transcript.contains("GATED_BYTES"),
        "`n` must not let the gated read through: {transcript}"
    );
}

// ---------------------------------------------------------------------------
// T3-S2: `--tui <MODE>`, `--no-alt-screen`, and the `[tui]` table reaching the
// product (design §4.8, plan row T3-S2).
//
// FAILING-FIRST: neither flag exists and `[tui]` is an unknown field on
// `Config` today, so every test below fails against this commit. The
// precedence and default assertions live in `config.rs`'s unit tests; these
// pin the user-visible surface: help text, argument rejection, and what
// `kitsu doctor` says about a misspelled key.
// ---------------------------------------------------------------------------

/// The flags are discoverable, with their accepted values, and `--tui` takes a
/// value while `--no-alt-screen` does not.
#[test]
fn test_help_exposes_the_tui_mode_flag_and_no_alt_screen() {
    let home = TempDir::new().unwrap();
    let mut cmd = base_cmd(home.path());
    cmd.arg("--help");
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains("--tui <MODE>"),
        "--tui must appear with its value name: {stdout}"
    );
    assert!(
        stdout.contains("--no-alt-screen"),
        "--no-alt-screen must appear: {stdout}"
    );
    // T5-S2: `fullscreen` has its renderer, so `--tui` advertises it. The
    // intent is unchanged — every mode the flag accepts is discoverable from
    // `--help` and no accepted mode is hidden — and the list is pinned whole
    // rather than by a `contains` per mode, so appending a fourth mode
    // without saying so here fails.
    assert!(
        stdout.contains("[possible values: classic, inline, fullscreen]"),
        "the accepted modes must be listed, fullscreen among them: {stdout}"
    );
}

/// An unknown mode on the command line is clap's own loud error, not a
/// fail-closed fallback to classic. The retired `KITSU_TUI` gate swallowed
/// `Inline` and `1`; a flag the user typed on purpose must not.
#[test]
fn test_tui_flag_rejects_an_unknown_mode_naming_the_value() {
    let home = TempDir::new().unwrap();
    // `fullscreen` left this list in T5-S2 and joined the accepted one
    // below: the mode now has a renderer, so the flag must take it. The
    // intent survives intact — a value typed on purpose is clap's own loud
    // error and never a silent fallback to classic — and the case typos stay
    // here, which is what keeps the move from being a relaxation.
    for value in [
        "Inline",
        "INLINE",
        "viewport",
        "Fullscreen",
        "FULLSCREEN",
        "1",
        "",
    ] {
        let output = base_cmd(home.path())
            .args(["--tui", value, "doctor"])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "--tui {value:?} must be rejected, not defaulted: {output:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            stderr.contains("invalid value") && stderr.contains("--tui"),
            "the error must name the flag and the bad value for {value:?}: {stderr}"
        );
    }

    // ...and the accepted values are accepted, so the loop above is not
    // rejecting every invocation. `fullscreen` is one of them now, and this
    // is the surface Decision 1 of T5-S2 is about: a mode a session can
    // switch to must be a mode it can *start* in.
    for value in ["classic", "inline", "fullscreen"] {
        let output = base_cmd(home.path())
            .args(["--tui", value, "doctor"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert!(
            !stderr.contains("invalid value"),
            "--tui {value} must parse: {stderr}"
        );
    }
}

/// `--no-alt-screen` is a bare switch and composes with `--tui`.
#[test]
fn test_no_alt_screen_is_a_bare_switch_that_composes_with_tui() {
    let home = TempDir::new().unwrap();
    let output = base_cmd(home.path())
        .args(["--tui", "inline", "--no-alt-screen", "doctor"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("invalid value"),
        "both flags must parse together: {stderr}"
    );
}

/// The risk gate of design §4.8, end to end: `kitsu doctor` names the
/// misspelled `[tui]` key. Today the whole table is unknown, so the message
/// names `tui` and the user is never told which key they got wrong.
#[test]
fn test_doctor_names_an_unknown_tui_key() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let config = home.path().join(".kitsu/config.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!("{contents}\n[tui]\nleft_arrow_opens_agent = true\n"),
    )
    .unwrap();

    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["config"]["status"], "parse/read failed");
    let issues = report["blocking_issues"]
        .as_array()
        .expect("doctor must report blocking issues")
        .iter()
        .map(|issue| issue.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        issues.contains("unknown field `left_arrow_opens_agent`"),
        "doctor must name the misspelled key: {issues}"
    );
    assert!(
        issues.contains("left_arrow_opens_agents"),
        "and the real key, which is the fix: {issues}"
    );
}

/// The converse, so the test above is not passing on a config that was broken
/// for some other reason: a well-spelled `[tui]` table parses.
#[test]
fn test_doctor_accepts_a_well_spelled_tui_table() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let config = home.path().join(".kitsu/config.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!(
            "{contents}\n[tui]\nmode = \"inline\"\nalternate_screen = \"always\"\n\
             prefix = \"ctrl+b\"\ntitle = false\nleft_arrow_opens_agents = false\n\
             cell_ring_lines = 2000\n"
        ),
    )
    .unwrap();

    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["config"]["status"], "parsed successfully");
}

/// **F62, file door, end to end.** `[tui] alternate_screen = "never"` is a
/// documented, parsed, validated value that nothing can honour: there is no
/// in-viewport overlay path, so the agent view takes the alternate screen
/// whatever this key says. Before this card a user could set it, have it
/// accepted, and watch the overlay take the alternate screen anyway.
///
/// So `doctor` must now FAIL on it and say what is accepted. This is the
/// deliberate converse of `test_doctor_accepts_a_well_spelled_tui_table`
/// above: that one proves a well-spelled table still parses, this one proves
/// the refusal reaches the user through the real binary rather than only
/// through a unit test -- the message has to survive the credential-hint
/// allow-list to get here at all.
#[test]
fn test_doctor_refuses_alternate_screen_never_naming_what_is_accepted() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");
    let config = home.path().join(".kitsu/config.toml");
    let contents = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!("{contents}\n[tui]\nalternate_screen = \"never\"\n"),
    )
    .unwrap();

    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["config"]["status"], "parse/read failed",
        "a value nothing can honour must not be reported as parsed: {report}"
    );
    let issues = report["blocking_issues"]
        .as_array()
        .expect("doctor must report blocking issues")
        .iter()
        .map(|issue| issue.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        issues.contains("`alternate_screen`"),
        "doctor must name the key: {issues}"
    );
    assert!(
        issues.contains("is not available yet"),
        "doctor must say the value is not available yet, not that it is \
         invalid -- the user should wait, not stop wanting it: {issues}"
    );
    assert!(
        issues.contains("accepted: `auto`, `always`"),
        "doctor must name the accepted set: {issues}"
    );
    // F48: the key and the permitted set, never the line the user wrote.
    assert!(
        !issues.contains("alternate_screen = "),
        "F48: the refusal must not quote the offending config line: {issues}"
    );
}

/// **F62, argv door, end to end.** `--no-alt-screen` is the same unimplemented
/// capability by another name: a live global switch documented as *"a one-way
/// force"* to `never`, which `README` promises will *"Force `[tui]
/// alternate_screen` to `never`"*. Since `never` cannot be honoured, the flag
/// cannot be honoured either.
///
/// Refusing the file and not the flag would MOVE the lie rather than close it,
/// and would leave the promise lying exactly where it hurts most: a user
/// passing this flag is in a multiplexer and believes they have disabled the
/// alternate screen.
///
/// The argv message MAY name the flag -- F48 withholds values read from a
/// config file, not a switch the user typed on the command line -- which is
/// the one way the two messages differ.
#[test]
fn test_no_alt_screen_is_refused_until_the_in_viewport_path_exists() {
    let home = TempDir::new().expect("temp home");
    write_config(home.path(), "http://127.0.0.1:1");

    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["--no-alt-screen", "doctor"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        !output.status.success(),
        "a flag kitsu cannot honour must not exit 0: {output:?}"
    );
    assert!(
        stderr.contains("--no-alt-screen"),
        "the argv refusal must name the flag the user typed: {stderr}"
    );
    assert!(
        stderr.contains("is not available yet"),
        "the flag must be refused as unavailable, not as invalid: {stderr}"
    );

    // Bidirectional: the identical invocation WITHOUT the flag still works,
    // so the refusal above is the flag's and not a broken fixture.
    let without = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["doctor"])
        .output()
        .unwrap();
    assert!(
        without.status.success(),
        "`doctor` without the flag must still succeed: {without:?}"
    );
}

// ---------------------------------------------------------------------------
// F120: a `--jsonl` run that drops a tool result must not report unqualified
// success. The tool runs, bounds its own output correctly and reports the
// truth; the history trimmer then replaces the whole message with a dead
// `[older tool output elided]` stub before the provider ever sees it — and
// today every event on stdout still says the run went fine.
//
// The distinction that shapes these tests: an eviction is only a LOSS when
// nothing recallable survives it. With a low `--offload-cap` the same eviction
// keeps the `fs_read_file` pointer line, which is the documented good case the
// flag exists to produce, and must stay silent.
// ---------------------------------------------------------------------------

/// Drive one `--jsonl` run in which a single large `fs_read_file` result is
/// evicted by the history trimmer before the second provider round.
///
/// `window` tunes the pressure: a small window forces the eviction, a large one
/// leaves the result intact. `extra_args` selects the eviction's character —
/// `--no-artifacts` suppresses the on-disk copy, so the stub is dead; a low
/// `--offload-cap` leaves a re-readable pointer, so the stub is recoverable.
async fn run_trimmed_tool_result_jsonl(window: &str, extra_args: &[&str]) -> TrimmedToolResultRun {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let input = work.path().join("evidence.txt");
    // Large enough that its result dominates the small-window budget, and
    // distinctive enough that the trimmer's stub is recognisable by absence.
    fs::write(&input, format!("HEAD{}TAIL", "evidence-line\n".repeat(900))).unwrap();

    let read_args = serde_json::json!({"path": input}).to_string();
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"tool_call_id\":\"big_read\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "reported on the evidence"},
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
                        "id": "big_read",
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
    let work_root = work.path().to_string_lossy().to_string();
    let mut args = vec![
        "--gateway",
        "example",
        "--jsonl",
        "--no-stream",
        "--context-window",
        window,
    ];
    args.extend_from_slice(extra_args);
    args.extend_from_slice(&[
        "--max-rounds",
        "3",
        "chat",
        "--no-session",
        "--allow-path",
        &work_root,
        "read the evidence file and report on it",
    ]);
    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .current_dir(work.path())
        .args(&args)
        .output()
        .unwrap();
    let events = String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let requests = server.received_requests().await.unwrap();
    // The content the SECOND request carried for the evicted call. This is the
    // ground truth for whether the run lost anything: the event stream is what
    // is under test, so it cannot also be the evidence that a loss occurred.
    let second_round_tool_content = requests
        .get(1)
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap())
        .and_then(|body| {
            body["messages"].as_array().and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message["tool_call_id"] == "big_read")
                    .and_then(|message| message["content"].as_str().map(str::to_string))
            })
        })
        .expect("the second provider request must carry the read's tool result");
    server.verify().await;
    TrimmedToolResultRun {
        output,
        events,
        second_round_tool_content,
    }
}

/// One `--jsonl` run of the eviction fixture: the process result, its parsed
/// event stream, and what the provider actually received for the evicted call.
struct TrimmedToolResultRun {
    output: std::process::Output,
    events: Vec<serde_json::Value>,
    second_round_tool_content: String,
}

/// The ordered `"type"` sequence a two-round tool run emits today. Enumerated
/// from the stream rather than searched for, so an additive event anywhere in
/// the run breaks this list instead of hiding behind a narrower assertion.
fn jsonl_event_types(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["type"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
#[serial]
async fn test_jsonl_reports_a_dropped_tool_result_with_a_count() {
    let run = run_trimmed_tool_result_jsonl("2048", &["--no-artifacts"]).await;
    let TrimmedToolResultRun {
        output,
        events,
        second_round_tool_content,
    } = run;
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    // The fix must not reach for stderr: this run's silence there is the
    // contract every other `--jsonl` test in this file already depends on.
    assert!(output.stderr.is_empty(), "{output:?}");

    // The loss really happened: the second request carried the dead stub, not
    // the evidence. Without this the test could pass on a run that lost
    // nothing, which is F61's failure mode.
    assert_eq!(
        second_round_tool_content, "[older tool output elided]",
        "fixture must actually drop the result, with nothing recallable left behind"
    );

    let dropped = events
        .iter()
        .find(|event| event["type"] == "tool_results_dropped")
        .unwrap_or_else(|| {
            panic!(
                "a run that replaced a tool result with `[older tool output elided]` reported \
                 no loss on stdout; event types were {:?}",
                jsonl_event_types(&events)
            )
        });
    assert_eq!(
        dropped["dropped"], 1,
        "the event must say HOW MANY results were dropped — three is a different \
         story from `something was trimmed`: {dropped}"
    );

    // The defect is a mismatch, not a silence: the tool told the truth and the
    // turn did not. Both halves are asserted so a fix that merely adds an
    // event while corrupting the tool receipt cannot pass.
    let read = events
        .iter()
        .find(|event| event["type"] == "tool_finished" && event["name"] == "fs_read_file")
        .expect("the large read's completion receipt");
    assert_eq!(
        read["success"], true,
        "the tool succeeded and must keep saying so: {read}"
    );
    assert_eq!(
        read["output_truncated"], true,
        "the tool bounded its own output correctly: {read}"
    );
    assert!(
        read["output_bytes"].as_u64().unwrap() > 11_000,
        "fixture must be large enough to force the eviction: {read}"
    );
}

/// IGNORED by the E11-onto-main reconciliation, with its original parameters
/// left in place so the intent is still readable.
///
/// This test needs a window tight enough that the trimmer strips an offloaded
/// result's *preview* while keeping its pointer. Main's default `git` tool
/// makes seven tools instead of six, and the larger schema raises the
/// protected prompt to ~1562 tokens: at the original 1536-token window the
/// context preflight now refuses the request before any trimming happens, and
/// at every window large enough to pass preflight (>= ~2270) the preview fits
/// inside the budget and is never stripped. Windows 2272/2288/2304/2320 and
/// offload caps 1024/4096/8192 were each measured; none reproduces the state.
///
/// So the band this test lived in no longer exists, and the fix is to
/// re-derive its pressure against the seven-tool schema rather than to retune
/// one number. Its sibling `test_jsonl_reports_a_dropped_tool_result...`
/// (window 2048 with `--no-artifacts`, a smaller schema) still passes and
/// still covers the dropped-result path.
#[ignore = "premise invalidated by main's seventh default tool; pressure needs re-deriving"]
#[tokio::test]
#[serial]
async fn test_jsonl_stays_silent_when_a_trimmed_tool_result_is_still_recallable() {
    // A low `--offload-cap` is the documented *good* case: the evicted result
    // leaves an `fs_read_file` pointer, so nothing is lost and a warning here
    // would be noise that teaches readers to ignore the real one.
    let run = run_trimmed_tool_result_jsonl("1536", &["--offload-cap", "1024"]).await;
    let TrimmedToolResultRun {
        output,
        events,
        second_round_tool_content,
    } = run;
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty(), "{output:?}");

    let types = jsonl_event_types(&events);
    assert_eq!(
        types,
        [
            "run_started",
            "turn_started",
            "provider_request",
            "provider_attempt",
            "tool_started",
            "tool_finished",
            "provider_request",
            "provider_attempt",
            "assistant",
            "assistant_disposition",
            "turn_finished",
            "final_summary",
        ],
        "a lossless eviction must leave the JSONL stream byte-identical to today"
    );
    let raw = String::from_utf8(output.stdout).unwrap();
    assert!(
        !raw.contains("tool_results_dropped") && !raw.contains("\"dropped\""),
        "a recallable eviction must not be reported as a loss: {raw}"
    );
    // Non-vacuity: the eviction this run is asserted to be silent about must
    // actually have happened, and must have left the pointer behind. A bare
    // pointer line with no preview is exactly `elided_tool_stub`'s lossless
    // branch — the body is gone, the recall path is not.
    assert!(
        second_round_tool_content.starts_with("[tool output "),
        "a low offload cap must leave an fs_read_file pointer the model can recall: \
         {second_round_tool_content}"
    );
    assert!(
        !second_round_tool_content.contains("Preview:"),
        "the preview body must have been evicted, or this run trimmed nothing and the \
         silence it asserts is vacuous: {second_round_tool_content}"
    );
}

#[tokio::test]
#[serial]
async fn test_jsonl_stays_silent_when_no_tool_result_is_trimmed_at_all() {
    // N1 pinned rather than assumed: a run with no eviction is exactly the
    // stream it is today, including after the lossy path gains an event.
    let run = run_trimmed_tool_result_jsonl("262144", &[]).await;
    let TrimmedToolResultRun {
        output,
        events,
        second_round_tool_content,
    } = run;
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        jsonl_event_types(&events),
        [
            "run_started",
            "turn_started",
            "provider_request",
            "provider_attempt",
            "tool_started",
            "tool_finished",
            "provider_request",
            "provider_attempt",
            "assistant",
            "assistant_disposition",
            "turn_finished",
            "final_summary",
        ],
        "a run that lost nothing must stay byte-identical to today"
    );
    let raw = String::from_utf8(output.stdout).unwrap();
    assert!(
        !raw.contains("tool_results_dropped"),
        "nothing was dropped, so nothing may be reported: {raw}"
    );
    // Non-vacuity in the other direction: this run must have trimmed nothing,
    // so the silence above is the no-loss guarantee and not a missed eviction.
    assert!(
        second_round_tool_content.contains("evidence-line")
            && !second_round_tool_content.contains("older tool output elided"),
        "a 256K window must carry the whole result through to the provider: \
         {} chars",
        second_round_tool_content.len()
    );
}

// ---------------------------------------------------------------------------
// F122: tell a HUMAN when a tool result was dropped.
//
// F120 shipped the machine half: a `--jsonl` run now emits
// `tool_results_dropped { round, dropped }` when history trimming replaced a
// whole tool result with a dead `[older tool output elided]` stub. A human
// running interactively still gets nothing — the tool ran, reported honestly,
// and its result vanished before the provider saw it in total silence.
//
// WHERE THE LINE BELONGS, measured rather than assumed. The only mid-turn
// human warning channel a plain `chat` run has is the runtime-notice sink in
// `live_agent_event_log_capturing`:
//
//     } else if let Some(notice) = format_runtime_notice(event) {
//         kitsu_render::finish_line();
//         eprintln!("~ {notice}");
//
// (`main.rs:14027`, with the same two lines again at `:14044` for slash-command
// runtimes and `:14081` for the buffered drain in `finish_agent_trace`.) A
// plain interactive chat has no agent view at all, so the two `ToolResultsDropped`
// no-op arms F120 left in `subagent.rs` / `subagent/thread.rs` mark where an
// *agent timeline* line would go — not where this card's line goes.
//
// THE GATE. That sink is NOT TTY-gated: it fires on any non-`--jsonl` run,
// piped included. N1 permits a line on an interactive TTY but requires all four
// of non-TTY, `--plain`/`NO_COLOR`, assistive and JSONL to stay byte-identical,
// so the new line needs a gate the existing `~` notices do not have. The four
// surfaces below pin that, and the lossless case pins that a recallable
// eviction stays silent on the human channel too.
//
// THE CONTRACT THESE TESTS PIN, kept as small as it can be so the wording stays
// the implementer's: on a styled TTY run that dropped results, the human
// channel carries EXACTLY ONE line per round naming the COUNT; on all four N1
// surfaces, and on every lossless or loss-free run, it carries none.
// ---------------------------------------------------------------------------

/// Carrier for one eviction run: how the child's descriptors are wired, which
/// is the whole variable this card turns on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DroppedResultCarrier {
    /// `.output()`: stdout, stderr and stdin all pipes. N1's "non-TTY".
    Piped,
    /// stdout, stderr and stdin all on one PTY, styling live, driven through
    /// the interactive REPL. This is the surface N1 does not cover.
    StyledPty,
}

/// One eviction run, whatever its carrier: the human-visible text, the JSONL
/// stream when there was one, and what the provider actually received for the
/// evicted call.
struct DroppedResultRun {
    /// Everything a human would have seen. For [`DroppedResultCarrier::Piped`]
    /// this is stderr; for the PTY it is the merged terminal transcript.
    human: String,
    /// stdout, parsed line-by-line, for `--jsonl` runs only.
    events: Vec<serde_json::Value>,
    /// Ground truth for whether the run lost anything. The human channel is
    /// what is under test, so it cannot also be the evidence of the loss.
    second_round_tool_content: String,
    success: bool,
    /// The fixture's temp working directory, so its path can be normalized out
    /// of a byte-identical pin.
    work_root: String,
}

/// Every line of the human channel that reports a drop, ENUMERATED rather than
/// searched for a prefix: the marker and wording stay the implementer's choice,
/// so the test keys on the one word the card is about.
///
/// Escape sequences are stripped first — on a styled PTY the line arrives
/// coloured, and `~ \x1b[33mdropped\x1b[0m` must still count as one line.
fn dropped_result_notices(human: &str) -> Vec<String> {
    strip_terminal_escapes(human)
        .lines()
        .map(str::trim)
        .filter(|line| line.to_ascii_lowercase().contains("dropped"))
        .map(str::to_string)
        .collect()
}

/// Remove CSI/OSC escape sequences and carriage returns so a styled PTY
/// transcript can be read as lines of text.
fn strip_terminal_escapes(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            match bytes.get(i) {
                // CSI: parameters, then one final byte in 0x40..=0x7e.
                Some(b'[') => {
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                // OSC: terminated by BEL or ST.
                Some(b']') => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != 0x07 {
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                // Two-byte escapes.
                Some(_) => i += 1,
                None => {}
            }
            continue;
        }
        if bytes[i] == b'\r' {
            out.push('\n');
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// The human channel reduced to the distinct lines a reader would actually
/// have seen, for use in failure messages.
///
/// A styled PTY transcript is ~7 KB of prompt redraw for a turn this short, and
/// a panic that dumps it buries its own finding. This keeps each distinct line
/// once, drops the keystroke-echo prefixes a longer line supersedes, and caps
/// the result - so the enumeration stays readable, which is the only form in
/// which "nothing said X" is worth reading.
fn human_channel_digest(human: &str) -> String {
    let stripped = strip_terminal_escapes(human);
    let mut kept: Vec<String> = Vec::new();
    for line in stripped.lines().map(str::trim).filter(|l| !l.is_empty()) {
        // A keystroke echo is a strict prefix of the next echo; keep the last.
        while kept
            .last()
            .is_some_and(|last| line.starts_with(last.as_str()))
        {
            kept.pop();
        }
        if !kept.iter().any(|seen| seen == line) {
            kept.push(line.to_string());
        }
    }
    let total = kept.len();
    if total > 40 {
        kept.truncate(40);
        kept.push(format!("... and {} further distinct line(s)", total - 40));
    }
    kept.join("\n")
}

/// Drive one run in which large `fs_read_file` results are evicted by the
/// history trimmer before the second provider round, over either carrier.
///
/// `window` tunes the pressure; `calls` is how many large reads the first
/// assistant message asks for (the per-round-vs-per-line question); `extra_args`
/// selects the eviction's character — `--no-artifacts` suppresses the on-disk
/// copy so the stub is dead, a low `--offload-cap` leaves a re-readable pointer.
async fn run_dropped_tool_result(
    carrier: DroppedResultCarrier,
    window: &str,
    calls: usize,
    extra_args: &[&str],
) -> DroppedResultRun {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let work = TempDir::new().unwrap();
    let mut tool_calls = Vec::new();
    for index in 0..calls {
        let input = work.path().join(format!("evidence-{index}.txt"));
        fs::write(&input, format!("HEAD{}TAIL", "evidence-line\n".repeat(900))).unwrap();
        tool_calls.push(serde_json::json!({
            "id": format!("big_read_{index}"),
            "type": "function",
            "function": {
                "name": "fs_read_file",
                "arguments": serde_json::json!({"path": input}).to_string()
            }
        }));
    }

    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .and(body_string_contains("\"tool_call_id\":\"big_read_0\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "reported on the evidence"},
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
                    "tool_calls": tool_calls
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
    let work_root = work.path().to_string_lossy().to_string();
    let work_root_for_run = work_root.clone();
    let mut args = vec![
        "--gateway",
        "example",
        "--no-stream",
        "--context-window",
        window,
    ];
    args.extend_from_slice(extra_args);
    args.extend_from_slice(&[
        "--max-rounds",
        "3",
        "chat",
        "--no-session",
        "--allow-path",
        &work_root,
    ]);

    let jsonl = extra_args.contains(&"--jsonl");
    let (human, stdout, success) = match carrier {
        DroppedResultCarrier::Piped => {
            let mut piped = args.clone();
            piped.push(PROMPT_READ_THE_EVIDENCE);
            let output = base_cmd(home.path())
                .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
                .current_dir(work.path())
                .args(&piped)
                .output()
                .unwrap();
            (
                String::from_utf8_lossy(&output.stderr).into_owned(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
                output.status.success(),
            )
        }
        DroppedResultCarrier::StyledPty => {
            let (status, transcript) = run_styled_chat_pty_turn(home.path(), work.path(), &args);
            (transcript.clone(), transcript, status.success())
        }
    };

    let events = if jsonl {
        stdout
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let requests = server.received_requests().await.unwrap();
    let second_round_tool_content = requests
        .get(1)
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body).unwrap())
        .and_then(|body| {
            body["messages"].as_array().and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message["tool_call_id"] == "big_read_0")
                    .and_then(|message| message["content"].as_str().map(str::to_string))
            })
        })
        .expect("the second provider request must carry the first read's tool result");
    server.verify().await;

    DroppedResultRun {
        human,
        events,
        second_round_tool_content,
        success,
        work_root: work_root_for_run,
    }
}

/// The one prompt every run in this block submits.
const PROMPT_READ_THE_EVIDENCE: &str = "read the evidence files and report on them";

/// Run one chat turn with styling LIVE on a PTY: stdin, stdout and stderr all
/// on the same terminal, no `--plain`, no `--jsonl`.
///
/// `tests/cli.rs`'s [`run_plain_interactive_chat_pty`] hardcodes `--plain`, so
/// it cannot see anything gated on a styled TTY; `tests/tui.rs`'s styled
/// harness points at an unreachable gateway and only ever submits slash
/// commands, so it cannot drive a tool turn. This is the smallest harness that
/// is both styled and backed by a real mock provider.
///
/// Driven on triggers found in the transcript, never on a sleep: the prompt
/// goes in once `kitsu>` appears, `/quit` once the assistant's answer does.
#[cfg(unix)]
fn run_styled_chat_pty_turn(
    home: &Path,
    cwd: &Path,
    args: &[&str],
) -> (std::process::ExitStatus, String) {
    use std::io::{Read as _, Write as _};
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
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0,
        "allocate styled drop-notice PTY"
    );
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
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .env_remove("KITSU_PLAIN")
        .env_remove("KITSU_SCREEN_READER")
        .env_remove("ANTHROPIC_TOKEN_EXPIRES")
        .current_dir(cwd)
        // SAFETY: all three live descriptors transfer exactly once.
        .stdin(unsafe { Stdio::from_raw_fd(child_stdin) })
        .stdout(unsafe { Stdio::from_raw_fd(child_stdout) })
        .stderr(unsafe { Stdio::from_raw_fd(slave) })
        .args(args);
    let mut child = command.spawn().expect("spawn styled drop-notice chat");
    // SAFETY: ownership of the remaining master descriptor transfers once.
    let mut terminal = unsafe { fs::File::from_raw_fd(master) };

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut transcript = Vec::new();
    let mut submitted = false;
    let mut quit = false;

    let status = loop {
        let mut buffer = [0_u8; 4096];
        match terminal.read(&mut buffer) {
            Ok(0) => {}
            Ok(read) => transcript.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {}
            Err(error) => panic!("read styled drop-notice PTY: {error}"),
        }
        let text = String::from_utf8_lossy(&transcript).into_owned();
        if !submitted && text.contains("kitsu>") {
            let _ = terminal.write_all(format!("{PROMPT_READ_THE_EVIDENCE}\n").as_bytes());
            let _ = terminal.flush();
            submitted = true;
        } else if submitted
            && !quit
            && (text.contains("reported on the evidence") || text.contains("! error:"))
        {
            let _ = terminal.write_all(b"/quit\n");
            let _ = terminal.flush();
            quit = true;
        }
        if let Some(status) = child.try_wait().expect("poll styled drop-notice chat") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "styled drop-notice chat timed out (submitted={submitted} quit={quit}): {}",
                String::from_utf8_lossy(&transcript)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    (status, String::from_utf8_lossy(&transcript).into_owned())
}

/// Every word the drop report could reasonably use, enumerated so an N1
/// surface cannot stay "silent" merely because the implementer picked a
/// synonym this file did not predict.
const DROP_VOCABULARY: [&str; 4] = ["dropped", "elided", "discarded", "lost"];

/// Assert the human channel says nothing at all about a drop, over the whole
/// enumerated vocabulary rather than one guessed marker.
fn assert_human_channel_silent_about_drops(run: &DroppedResultRun, surface: &str) {
    let text = strip_terminal_escapes(&run.human).to_ascii_lowercase();
    for word in DROP_VOCABULARY {
        assert!(
            !text.contains(word),
            "N1 requires the {surface} surface to stay byte-identical to today, and today it \
             says nothing about a dropped tool result; it now contains {word:?}. The \
             channel's distinct lines were:\n{}",
            human_channel_digest(&run.human)
        );
    }
}

/// Assert the fixture really destroyed the result, from the provider's side.
/// Without this every silence assertion above could pass on a run that trimmed
/// nothing (F61).
fn assert_result_really_dropped(run: &DroppedResultRun) {
    assert_eq!(
        run.second_round_tool_content, "[older tool output elided]",
        "fixture must actually drop the result with nothing recallable left behind, or every \
         assertion about reporting that drop is vacuous"
    );
}

/// Assert the turn actually ran to completion on a PTY carrier: without this a
/// harness that hung before the tool call would report a convincing silence.
fn assert_styled_turn_completed(run: &DroppedResultRun) {
    let digest = human_channel_digest(&run.human);
    assert!(run.success, "styled turn must exit 0:\n{digest}");
    let text = strip_terminal_escapes(&run.human);
    assert!(
        text.contains("reported on the evidence"),
        "the assistant's answer must have reached the terminal, or the silence this test \
         measures is the harness's and not the binary's:\n{digest}"
    );
    assert!(
        text.contains("evidence-0.txt"),
        "the large read must have run and been shown, or nothing was there to drop:\n{digest}"
    );
}

/// **The card.** A human on an interactive styled TTY whose tool result was
/// destroyed before the provider saw it is told so, once, with the count.
///
/// N1 permits this: its four byte-identical surfaces are non-TTY, `--plain`/
/// `NO_COLOR`, assistive and JSONL, and an interactive TTY run is none of them.
/// The three `test_n1_*` tests below hold that line.
#[tokio::test]
#[serial]
async fn test_a_styled_tty_run_tells_the_human_a_tool_result_was_dropped() {
    let run = run_dropped_tool_result(
        DroppedResultCarrier::StyledPty,
        "2560",
        1,
        &["--no-artifacts"],
    )
    .await;
    assert_styled_turn_completed(&run);
    assert_result_really_dropped(&run);

    let notices = dropped_result_notices(&run.human);
    assert_eq!(
        notices.len(),
        1,
        "a styled interactive run that destroyed a tool result told the human nothing: the \
         tool reported honestly, the result vanished before the provider saw it, and the \
         session was silent. Expected exactly one line reporting the drop; the terminal \
         carried {}. The channel's distinct lines were:\n{}",
        notices.len(),
        human_channel_digest(&run.human)
    );
    assert!(
        notices[0].contains('1'),
        "the line must name HOW MANY results were dropped - \"three results were dropped\" is \
         a different story from \"something was trimmed\": {:?}",
        notices[0]
    );
}

/// Per ROUND, with the count - not per result. Measured from the machine half
/// rather than chosen: two results destroyed in one round of budget
/// enforcement produce exactly one `tool_results_dropped` event carrying
/// `dropped: 2`, so one human line carrying 2 is the shape that matches it.
/// Two lines would overstate the number of times context was lost.
#[tokio::test]
#[serial]
async fn test_a_styled_tty_run_reports_one_line_per_round_naming_the_count() {
    let run = run_dropped_tool_result(
        DroppedResultCarrier::StyledPty,
        "2560",
        2,
        &["--no-artifacts"],
    )
    .await;
    assert_styled_turn_completed(&run);
    assert_result_really_dropped(&run);

    let notices = dropped_result_notices(&run.human);
    assert_eq!(
        notices.len(),
        1,
        "two results destroyed in ONE round of trimming must produce ONE line, not one per \
         result; the terminal carried {:?}. The channel's distinct lines were:\n{}",
        notices,
        human_channel_digest(&run.human)
    );
    assert!(
        notices[0].contains('2'),
        "the single line must carry the count 2, or a reader cannot tell one loss from two: \
         {:?}",
        notices[0]
    );
}

/// The documented GOOD case stays silent. A low `--offload-cap` evicts the
/// preview and leaves an `fs_read_file` pointer the model can still recall, so
/// nothing was lost; a warning here would be the noise that teaches a reader to
/// ignore the real one. This test passes today and must keep passing.
#[tokio::test]
#[serial]
async fn test_a_styled_tty_run_stays_silent_when_the_trimmed_result_is_recallable() {
    // Measured on THIS carrier rather than borrowed from the piped fixture: an
    // interactive turn's protected prompt is larger, so F120's 1536 trips the
    // context preflight here and never reaches the trimmer at all. 2048 is the
    // window at which the eviction happens and the pointer survives; 3072 and
    // above do not trim at all.
    let run = run_dropped_tool_result(
        DroppedResultCarrier::StyledPty,
        "2304",
        1,
        &["--offload-cap", "1024"],
    )
    .await;
    assert_styled_turn_completed(&run);
    // Non-vacuity, both halves: the eviction happened, and it left the pointer.
    assert!(
        run.second_round_tool_content.starts_with("[tool output "),
        "a low offload cap must leave an fs_read_file pointer the model can recall: {:?}",
        run.second_round_tool_content
    );
    assert!(
        !run.second_round_tool_content.contains("Preview:"),
        "the preview body must have been evicted, or this run trimmed nothing and the silence \
         it asserts is vacuous: {:?}",
        run.second_round_tool_content
    );
    assert_human_channel_silent_about_drops(&run, "lossless-eviction");
}

/// A run that drops nothing says nothing. Passes today and must keep passing.
#[tokio::test]
#[serial]
async fn test_a_styled_tty_run_stays_silent_when_no_result_is_trimmed_at_all() {
    let run = run_dropped_tool_result(DroppedResultCarrier::StyledPty, "8192", 1, &[]).await;
    assert_styled_turn_completed(&run);
    // Fences the 8192 threshold: if drift makes this window lossy, this fails
    // loudly instead of turning the silence below into a tautology.
    assert!(
        run.second_round_tool_content.contains("evidence-line"),
        "an 8192 window must carry the whole result through to the provider, or this test \
         asserts silence about a loss it did not intend: {:?}",
        run.second_round_tool_content
    );
    assert_human_channel_silent_about_drops(&run, "no-trim");
}

/// **N1, surface 1 of 4: non-TTY.** A piped run's stderr is empty today and
/// must stay empty - 78 assertions in this file depend on exactly that.
#[tokio::test]
#[serial]
async fn test_n1_non_tty_stays_byte_identical_when_a_tool_result_is_dropped() {
    let run =
        run_dropped_tool_result(DroppedResultCarrier::Piped, "2048", 1, &["--no-artifacts"]).await;
    assert!(run.success, "piped run must exit 0: {:?}", run.human);
    assert_result_really_dropped(&run);
    // A piped run's stderr is NOT empty - it carries six lines today - so the
    // pin is that text itself, with only the temp path, counts, ids and
    // timings normalized. A new drop line anywhere in it fails here.
    assert_eq!(
        normalize_volatile(&run.human, &run.work_root),
        NON_TTY_STDERR_TODAY,
        "N1: a non-TTY run's stderr must stay byte-identical to today, ids and timings \
         aside; it read:\n{}",
        run.human
    );
}

/// Every line a piped eviction run writes to stderr today, normalized. Written
/// out in full rather than searched, so a new line cannot slip in beside the
/// ones this test knows about.
const NON_TTY_STDERR_TODAY: &str = concat!(
    "~ artifacts disabled: commands keep bounded previews only; large tool results are not",
    " offloaded; existing artifacts are unchanged\n",
    "\u{25c6} Read(<WORK>/evidence-N.txt)\n",
    "  \u{25c7} N lines \u{b7} Nms\n",
    "kitsu-context: {\"used\":N,\"effective\":N,\"window\":N}\n",
    "~ run summary: N successful operation, N file(s) read, N file(s) written",
    " (N change(s)), session not saved\n",
    "kitsu-timing: {\"run_id\":\"N-N\",\"duration_ms\":N,\"status\":\"ok\",",
    "\"outcome\":\"succeeded\",\"assistant\":\"committed\"}\n",
);

/// Collapse the parts of a human channel that legitimately differ run to run -
/// the temp working directory, and every run of digits (ids, counts, timings) -
/// so the rest can be pinned exactly.
fn normalize_volatile(text: &str, work_root: &str) -> String {
    let replaced = text.replace(work_root, "<WORK>");
    let mut out = String::with_capacity(replaced.len());
    let mut in_digits = false;
    for ch in replaced.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('N');
                in_digits = true;
            }
            continue;
        }
        in_digits = false;
        out.push(ch);
    }
    out
}

/// **N1, surface 2 of 4: `--plain`/`NO_COLOR`.** Driven on a real PTY, because
/// `--plain` on a pipe is already surface 1: the only way to test the plain
/// surface distinctly is a terminal with styling turned off.
#[tokio::test]
#[serial]
async fn test_n1_plain_stays_byte_identical_when_a_tool_result_is_dropped() {
    let run = run_dropped_tool_result(
        DroppedResultCarrier::StyledPty,
        "2560",
        1,
        &["--plain", "--no-artifacts"],
    )
    .await;
    assert_styled_turn_completed(&run);
    assert_result_really_dropped(&run);
    assert_human_channel_silent_about_drops(&run, "--plain");
}

/// **N1, surface 3 of 4: assistive (`--screen-reader`).**
#[tokio::test]
#[serial]
async fn test_n1_screen_reader_stays_byte_identical_when_a_tool_result_is_dropped() {
    let run = run_dropped_tool_result(
        DroppedResultCarrier::StyledPty,
        "2560",
        1,
        &["--screen-reader", "--no-artifacts"],
    )
    .await;
    assert_styled_turn_completed(&run);
    assert_result_really_dropped(&run);
    assert_human_channel_silent_about_drops(&run, "--screen-reader");
}

/// **N1, surface 4 of 4: JSONL.** This card must add nothing to the stream F120
/// settled, and must not reach for stderr. The type sequence is enumerated, so
/// an additive event anywhere in the run fails here rather than hiding.
#[tokio::test]
#[serial]
async fn test_n1_jsonl_gains_nothing_from_the_human_report_of_a_dropped_result() {
    let run = run_dropped_tool_result(
        DroppedResultCarrier::Piped,
        "2048",
        1,
        &["--jsonl", "--no-artifacts"],
    )
    .await;
    assert!(run.success, "jsonl run must exit 0");
    assert_result_really_dropped(&run);
    assert_eq!(
        run.human, "",
        "N1: a --jsonl run's stderr is empty today and the human report must not reach for it"
    );
    let types = run
        .events
        .iter()
        .map(|event| event["type"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        types,
        [
            "run_started",
            "turn_started",
            "provider_request",
            "provider_attempt",
            "tool_started",
            "tool_finished",
            "tool_results_dropped",
            "provider_request",
            "provider_attempt",
            "assistant",
            "assistant_disposition",
            "turn_finished",
            "final_summary",
        ],
        "the human-facing report must leave F120's JSONL stream exactly as it is"
    );
}

// ---------------------------------------------------------------------------
// Card H2-title, requirement N1's non-TTY descriptor: the OSC terminal title
// is control bytes on **stderr**, so a run whose stderr is not a terminal must
// not receive it.
//
// The positive surface and the plain/assistive negatives are PTY measurements
// and live in `tests/tui.rs`; this is the piped one, which no PTY harness can
// make.
// ---------------------------------------------------------------------------

/// **A piped run receives no terminal title, while the bytes it receives
/// today are unchanged.**
///
/// Non-vacuous in both directions, and the second assertion is the
/// load-bearing one: this run **does** take the alternate screen and **does**
/// push the title stack today (`--tui fullscreen` reaches
/// `tui_bridge::enter_fullscreen` whatever the descriptors are), so the
/// sequences that must stay are asserted present and only the OSC 2 string is
/// asserted absent. A gate that suppressed the push as well would be an N1
/// violation rather than a fix.
#[test]
fn test_tui_title_is_not_written_to_a_non_tty_stderr() {
    let home = TempDir::new().unwrap();
    write_config(home.path(), "https://gateway.invalid");
    let output = base_cmd(home.path())
        .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
        .args(["--gateway", "example", "--tui", "fullscreen", "chat"])
        .arg("--no-session")
        .write_stdin("hello\n")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert!(
        stderr.contains("\x1b[22;2t") && stderr.contains("\x1b[?1049h"),
        "NON-VACUOUS: this run still enters a fullscreen segment on a piped \
         stderr, exactly as it does today; stderr:\n{}",
        stderr.escape_debug()
    );
    assert!(
        !stderr.contains("\x1b]2;"),
        "requirement N1, per descriptor: a non-terminal stderr must receive \
         no OSC terminal title; stderr:\n{}",
        stderr.escape_debug()
    );
    assert!(
        !stdout.contains("\x1b]2;"),
        "and the title is never written to stdout at all, which is why JSONL \
         is unaffected by construction; stdout:\n{}",
        stdout.escape_debug()
    );
}

// ---------------------------------------------------------------------------
// Card F57-reach: reachability, made structural.
//
// **F57** measured the defect this sweep exists for: a `SLASH_COMMANDS` entry
// with no dispatch arm, with all 824 binary and 245 CLI tests passing. The
// guard F57 itself wanted was unwritable in-process, because `handle_slash` is
// one `match` whose recognition *is* its execution — there is no stage, flag
// or return value that says "recognised" without also running `/quit`,
// `/clear` or `/fork`. So this sweep separates them the only honest way: it
// runs the real dispatcher, once per command, in its own process and its own
// temp HOME, and asks the only observable that defines unreachability — the
// unknown-command fallback.
//
// And it reads the registry exactly once, for its subjects. The check is
// against the binary's behaviour, never against the table again (F57's trap:
// `closest_slash_command` reads `SLASH_COMMANDS`, so any registry-vs-registry
// sweep passes vacuously).
// ---------------------------------------------------------------------------

/// Every registry entry, driven through the idle dispatcher; none of them may
/// reach `unknown command: /<name>`.
///
/// Subjects are enumerated from `SLASH_COMMANDS` at runtime, through the
/// `commands` array `/help` publishes in its JSONL payload — the registry is
/// private to the binary, and this is its own account of itself. **No command
/// name is typed in this test**, so a new entry becomes a subject the moment
/// it is registered, and no curated list can fall behind the table (F124,
/// F126, F131, F142) and no count can be wrong.
///
/// Covered: the top-level word of every entry, aliases included, since an
/// alias is a first-class registry entry and dispatches through the same
/// `match`. Deliberately **not** covered: arguments and subcommands (a
/// subcommand that falls through refuses inside its own arm and never reaches
/// this fallback), and *what* each arm does — this asks only whether anything
/// is listening. The mid-turn half is
/// `every_registered_command_typed_mid_turn_is_handled_or_queued_verbatim`
/// in the binary's own suite.
#[test]
fn every_registered_slash_command_reaches_a_dispatch_arm() {
    /// Run one single-line script and return stdout and stderr joined.
    fn run_one_command(line: &str) -> String {
        let home = TempDir::new().expect("temp home");
        write_config(home.path(), "http://127.0.0.1:1");
        let script = home.path().join("one-command.txt");
        fs::write(&script, format!("{line}\n")).expect("write script");
        let output = base_cmd(home.path())
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args([
                "--gateway",
                "example",
                "--jsonl",
                "chat",
                "--script",
                &script.to_string_lossy(),
            ])
            .output()
            .unwrap_or_else(|error| panic!("run {line:?}: {error}"));
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    // Subjects, from the registry's own publication of itself.
    let names: Vec<String> = run_one_command("/help")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|event| {
            let result = event.get("result")?.clone();
            (result["kind"] == "help").then(|| result["commands"].clone())
        })
        .expect("/help publishes its registry in the JSONL command result")
        .as_array()
        .expect("`commands` is an array")
        .iter()
        .map(|command| {
            command["name"]
                .as_str()
                .expect("every registry entry has a name")
                .to_string()
        })
        .collect();
    assert!(
        names.len() > 1,
        "the registry must have supplied the subjects: {names:?}"
    );

    // The detector, proven in this very run: a word no entry can hold must
    // reach the fallback. Derived from a registered name so that not even the
    // sentinel is a typed command name.
    let sentinel = format!("{}-is-not-a-registered-command", names[0]);
    let probe = run_one_command(&format!("/{sentinel}"));
    assert!(
        probe.contains(&format!("unknown command: /{sentinel}")),
        "NON-VACUOUS: an unregistered word still reaches the unknown-command \
         fallback, so the absence asserted below is a real dispatch and not a \
         harness that never looked; output:\n{probe}"
    );

    let mut undispatched: Vec<String> = Vec::new();
    for name in &names {
        let output = run_one_command(&format!("/{name}"));
        if output.contains(&format!("unknown command: /{name}")) {
            undispatched.push(name.clone());
        }
    }
    assert!(
        undispatched.is_empty(),
        "registered but undispatched (F57): {undispatched:?} — each is in \
         SLASH_COMMANDS with no arm in `handle_slash`, so typing it prints \
         `unknown command` between turns and, mid-turn, banks it as a future \
         prompt for the model. All {} registry entries were driven.",
        names.len()
    );
}

/// F51 byte contract: with tools enabled (`chat`'s default) a streamed prose answer
/// must reach `--plain` stdout as exactly the answer plus one terminating
/// newline — whether it is buffered to the end of the round or streamed
/// fragment by fragment. This is the test that catches a fix which streams the
/// fragments and then ALSO reprints the buffered text at the end of the round:
/// the answer would appear twice.
#[tokio::test]
#[serial]
async fn test_plain_streamed_answer_with_tools_enabled_is_printed_exactly_once() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"Here "},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"is "},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"the "},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"answer."},"finish_reason":null}]}"#,
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
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--plain", "chat", "stream me an ordinary answer"])
            .output()
            .expect("run a plain streamed chat")
    })
    .await
    .expect("join the ask process");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout, "Here is the answer.\n",
        "the --plain byte contract for a streamed answer with tools offered"
    );
    let requests = server
        .received_requests()
        .await
        .expect("the mock gateway records requests");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(
        body["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "F51's premise: this very round advertised tools: {body}"
    );
    server.verify().await;
}

/// F51's highest-risk case, and the realistic daily one:
/// `test_text_protocol_ignores_prose`'s shape — prose, then a braced tail that
/// could still have opened a text-protocol tool call, then more prose. The head
/// is disqualified at once and streams; the `{` tail can only be released when
/// the round ends. So this is the round where part of the text has streamed and
/// part has not, and an end-of-round site that reprints the WHOLE buffered
/// message instead of only the withheld tail duplicates the head. `print_message`
/// writes straight to the process descriptor and bypasses the renderer's surface
/// seam, so only a real `--plain` run can see that duplication.
#[tokio::test]
#[serial]
async fn test_plain_stream_disqualified_midway_prints_each_part_exactly_once() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sse = [
        r#"data: {"choices":[{"index":0,"delta":{"content":"Use a HashMap "},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"{ key: value } "},"finish_reason":null}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"for this."},"finish_reason":null}]}"#,
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
        .expect(1)
        .mount(&server)
        .await;

    let home = TempDir::new().expect("temp home");
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        let mut cmd = base_cmd(&home_path);
        cmd.env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--plain", "chat", "answer with a brace in the middle"])
            .output()
            .expect("run a plain streamed chat")
    })
    .await
    .expect("join the chat process");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout, "Use a HashMap { key: value } for this.\n",
        "a round disqualified midway must print its streamed head and its \
         withheld tail exactly once each"
    );
    assert_eq!(
        stdout.matches("Use a HashMap").count(),
        1,
        "the streamed head must not be reprinted with the buffered tail: {stdout:?}"
    );
    server.verify().await;
}

#[tokio::test]
#[serial]
async fn test_streaming_output_cap_prints_the_truncation_marker() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    let delta = serde_json::json!({"choices":[{"index":0,"delta":{"content":"partial streamed answer"},"finish_reason":null}]});
    let done = serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"length"}]});
    Mock::given(method("POST"))
        .and(path(CHAT_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!("data: {delta}\n\ndata: {done}\n\ndata: [DONE]\n\n"),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new().unwrap();
    write_config(home.path(), &server.uri());
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        base_cmd(&home_path)
            .env("ANTHROPIC_AUTH_TOKEN", TOKEN)
            .args(["--max-output-tokens", "12", "-p", "give a long answer"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("partial streamed answer"), "{stdout}");
    assert_eq!(
        stdout
            .matches("[truncated: model output length limit reached]")
            .count(),
        1,
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn test_interactive_git_is_default_without_enabling_general_commands() {
    use std::io::Write;
    for (flags, want_git, want_commands) in [
        (vec![], true, false),
        (vec!["--no-tools"], false, false),
        (vec!["--enable-commands"], true, true),
    ] {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_config(home.path(), "http://127.0.0.1:1");
        let mut sent = false;
        let mut quit = false;
        let (status, transcript) = run_plain_interactive_chat_pty(
            home.path(),
            project.path(),
            &flags,
            |output, terminal| {
                let text = String::from_utf8_lossy(output);
                if !sent && text.contains("kitsu>") {
                    terminal.write_all(b"/tools\n").unwrap();
                    terminal.flush().unwrap();
                    sent = true;
                }
                if sent
                    && !quit
                    && text.contains("enabled; permissions")
                    && text.matches("kitsu>").count() >= 2
                {
                    terminal.write_all(b"/quit\n").unwrap();
                    terminal.flush().unwrap();
                    quit = true;
                }
            },
        );
        assert!(status.success(), "{transcript}");
        let listed_git = transcript.lines().any(|line| line.trim() == "- git");
        let listed_commands = transcript
            .lines()
            .any(|line| line.trim() == "- command_exec");
        assert_eq!(listed_git, want_git, "{flags:?}: {transcript}");
        assert_eq!(listed_commands, want_commands, "{flags:?}: {transcript}");
    }
}
