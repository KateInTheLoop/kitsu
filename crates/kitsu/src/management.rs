//! Provider-free configuration, credential, session, and artifact commands.

use crate::cli::{AuthCommand, Cli, SessionCommand};
use crate::config::{Config, ConfigError, Gateway};
use kitsu_providers::auth::EnvBearerAuth;

use crate::agent_state_file;
use crate::artifacts;
use crate::chat::run_chat;
use crate::chat::ChatInvocation;
use crate::cli;
use crate::cli_error::CliError;
use crate::credential_store;
use crate::presentation::shorten_home;
use crate::presentation::terminal_inline;
use crate::runtime_state::runtime_state_dir;
use crate::runtime_state::runtime_state_dir_read_only;
use crate::session;
use crate::session_catalog::filter_sessions;
use crate::session_catalog::format_session_summaries;
use crate::setup::build_auth;

// Kept byte-for-byte aligned with README's "Ollama (local)" provider recipe
// (flavor, base_url, and model) so `kitsu init` never disagrees with the
// docs a new user is reading one section away, and so the generated config
// works out of the box against `ollama pull llama3.2` without a model-tag
// mismatch (cycle-8 docs-drift finding: the old `qwen2.5-coder:7b` default
// 404'd against the tag Ollama actually produces). `flavor` is validated
// against a closed set (`openai`/`anthropic`/`ollama`) but every value
// currently routes through the same OpenAI-compatible transport — `openai`
// here matches every other recipe, including this same Ollama gateway.
pub(crate) const STARTER_CONFIG_BODY: &str = r#"default_gateway = "ollama"
# Optional privacy default: create no automatic command/tool-output artifacts.
# artifact_storage = false

[gateways.ollama]
flavor = "openai"
base_url = "http://localhost:11434"
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
# Pull this model first (ollama pull llama3.2) or set one you already have
model = "llama3.2"
context_window = 32768
auth = { method = "none" }
"#;

pub(crate) fn starter_config(destination: &std::path::Path) -> String {
    format!(
        "# Kitsu starter configuration (contains no credentials).\n# Next: kitsu --config {} doctor\n{STARTER_CONFIG_BODY}",
        shell_quote(&destination.to_string_lossy()),
    )
}

/// Infer the export format from the destination extension. Only `.json` (case
/// insensitively) selects JSON; everything else keeps the Markdown default, so
/// the inference can never surprise an existing `.md`/extensionless workflow.
pub(crate) fn inferred_export_format(
    destination: &std::path::Path,
) -> session::SessionExportFormat {
    let is_json = destination
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
    if is_json {
        session::SessionExportFormat::Json
    } else {
        session::SessionExportFormat::Markdown
    }
}

pub(crate) fn reject_jsonl_for_management(cli: &Cli, replacement: &str) -> Result<(), CliError> {
    if cli.jsonl {
        Err(CliError::user(format!(
            "--jsonl is reserved for chat automation events; use {replacement} for this command"
        )))
    } else {
        Ok(())
    }
}

/// True when an output failure is only "the reader stopped reading". A closed
/// consumer (`| head -1`) is a normal shell idiom, not a kitsu error, and must
/// not be reported as one. The JSONL event sink keeps its own stricter contract.
pub(crate) fn output_error_is_broken_pipe(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::BrokenPipe
}

pub(crate) fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = serde_json::to_writer_pretty(&mut stdout, value) {
        if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) {
            return Ok(());
        }
        return Err(CliError::user(format!(
            "failed to encode JSON output: {error}"
        )));
    }
    if let Err(error) = stdout.write_all(b"\n") {
        if output_error_is_broken_pipe(&error) {
            return Ok(());
        }
        return Err(CliError::user(format!("failed to write output: {error}")));
    }
    match stdout.flush() {
        Ok(()) => Ok(()),
        Err(error) if output_error_is_broken_pipe(&error) => Ok(()),
        Err(error) => Err(CliError::user(format!("failed to flush output: {error}"))),
    }
}

pub(crate) fn run_init(
    cli: &Cli,
    requested: Option<&std::path::Path>,
    json: bool,
) -> Result<(), CliError> {
    use std::io::Write as _;

    reject_jsonl_for_management(cli, "kitsu init --json")?;
    let destination = requested
        .map(std::path::Path::to_path_buf)
        .or_else(|| cli.config.as_deref().map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("kitsu.toml"));
    if let Some(parent) = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            CliError::user(format!(
                "cannot create config directory '{}': {error}",
                parent.display()
            ))
        })?;
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&destination).map_err(|error| {
        let detail = if error.kind() == std::io::ErrorKind::AlreadyExists {
            "already exists; refusing to overwrite it".to_string()
        } else {
            error.to_string()
        };
        CliError::user(format!(
            "cannot initialize config '{}': {detail}",
            destination.display()
        ))
    })?;
    let contents = starter_config(&destination);
    let write_result = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all());
    if let Err(error) = write_result {
        drop(file);
        let cleanup = std::fs::remove_file(&destination);
        let cleanup_note = cleanup
            .err()
            .map(|cleanup| format!("; partial file cleanup also failed: {cleanup}"))
            .unwrap_or_default();
        return Err(CliError::user(format!(
            "cannot write config '{}': {error}{cleanup_note}",
            destination.display()
        )));
    }

    if json {
        print_json(&serde_json::json!({
            "created": true,
            "path": destination,
            "contains_credentials": false,
            "next": ["kitsu", "--config", destination, "doctor"],
        }))
    } else {
        println!(
            "created no-secret starter config at {}",
            terminal_inline(&destination.to_string_lossy())
        );
        let next = format!(
            "kitsu --config {} doctor",
            shell_quote(&destination.to_string_lossy())
        );
        println!("next: {}", terminal_inline(&next));
        Ok(())
    }
}

pub(crate) fn trim_secret_line_ending(
    mut bytes: zeroize::Zeroizing<Vec<u8>>,
) -> zeroize::Zeroizing<Vec<u8>> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    bytes
}

pub(crate) fn read_secret_stdin() -> Result<zeroize::Zeroizing<Vec<u8>>, CliError> {
    use std::io::Read as _;
    let limit = credential_store::MAX_CREDENTIAL_BYTES.saturating_add(2);
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(limit.min(4096)));
    std::io::stdin()
        .lock()
        .take(u64::try_from(limit).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| CliError::user(format!("cannot read managed credential: {error}")))?;
    if bytes.len() >= limit {
        return Err(CliError::user(
            "managed credential exceeds the 16 KiB safety limit",
        ));
    }
    Ok(trim_secret_line_ending(bytes))
}

#[cfg(unix)]
pub(crate) struct TerminalEchoGuard {
    pub(crate) fd: std::os::fd::RawFd,
    pub(crate) original: libc::termios,
}

#[cfg(unix)]
impl Drop for TerminalEchoGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

#[cfg(unix)]
pub(crate) async fn read_secret_tty() -> Result<zeroize::Zeroizing<Vec<u8>>, CliError> {
    use std::io::{Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        return Err(CliError::user(
            "auth login requires a terminal for hidden input; use --token-stdin for a secret-manager pipeline",
        ));
    }
    let fd = unsafe { libc::dup(libc::STDIN_FILENO) };
    if fd < 0 {
        return Err(CliError::user(
            "cannot duplicate terminal input; use --token-stdin instead",
        ));
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0
    {
        return Err(CliError::user(
            "cannot prepare terminal input; use --token-stdin instead",
        ));
    }
    let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(file.as_raw_fd(), original.as_mut_ptr()) } != 0 {
        return Err(CliError::user(
            "cannot prepare hidden terminal input; use --token-stdin instead",
        ));
    }
    let original = unsafe { original.assume_init() };
    let mut hidden = original;
    hidden.c_lflag &= !libc::ECHO;
    if unsafe { libc::tcsetattr(file.as_raw_fd(), libc::TCSAFLUSH, &hidden) } != 0 {
        return Err(CliError::user(
            "cannot disable terminal echo; use --token-stdin instead",
        ));
    }
    let echo_guard = TerminalEchoGuard {
        fd: file.as_raw_fd(),
        original,
    };
    std::eprint!("credential: ");
    std::io::stderr()
        .flush()
        .map_err(|error| CliError::user(format!("cannot write credential prompt: {error}")))?;

    let async_file = tokio::io::unix::AsyncFd::new(file)
        .map_err(|error| CliError::user(format!("cannot monitor terminal input: {error}")))?;
    let read = async {
        let limit = credential_store::MAX_CREDENTIAL_BYTES.saturating_add(2);
        let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(limit.min(4096)));
        loop {
            let mut ready = async_file.readable().await.map_err(|error| {
                CliError::user(format!("cannot wait for managed credential input: {error}"))
            })?;
            let mut chunk = [0_u8; 1024];
            match ready.try_io(|inner| {
                let mut file = inner.get_ref();
                file.read(&mut chunk)
            }) {
                Ok(Ok(0)) => break,
                Ok(Ok(read)) => {
                    bytes.extend_from_slice(&chunk[..read]);
                    if bytes.contains(&b'\n') {
                        break;
                    }
                    if bytes.len() >= limit {
                        return Err(CliError::user(
                            "managed credential exceeds the 16 KiB safety limit",
                        ));
                    }
                }
                Ok(Err(error)) => {
                    return Err(CliError::user(format!(
                        "cannot read managed credential: {error}"
                    )))
                }
                Err(_) => continue,
            }
        }
        Ok(trim_secret_line_ending(bytes))
    };
    let result = tokio::select! {
        result = read => result,
        _ = tokio::signal::ctrl_c() => Err(CliError::cancelled("auth login cancelled; no credential was stored")),
    };
    drop(echo_guard);
    std::eprintln!();
    result
}

#[cfg(not(unix))]
pub(crate) async fn read_secret_tty() -> Result<zeroize::Zeroizing<Vec<u8>>, CliError> {
    Err(CliError::user(
        "hidden terminal credential input is unavailable on this platform; use --token-stdin",
    ))
}

pub(crate) fn environment_override_present(gateway: &Gateway) -> bool {
    gateway.auth.method == "bearer-env"
        && std::env::var_os(&gateway.auth.env).is_some_and(|value| !value.is_empty())
}

pub(crate) fn require_persistent_credential_home() -> Result<(), CliError> {
    let Some(home) = kitsu_tools::host_home_dir() else {
        return Err(CliError::user(
            "auth login requires an absolute home directory (HOME, or USERPROFILE on Windows) so credentials are never persisted in temporary fallback state",
        ));
    };
    if !std::path::Path::new(&home).is_absolute() {
        return Err(CliError::user(
            "auth login requires an absolute HOME so credentials are never persisted in temporary fallback state",
        ));
    }
    Ok(())
}

pub(crate) fn managed_credential(
    state_dir: &std::path::Path,
    gateway_name: &str,
    gateway: &Gateway,
) -> Result<Option<zeroize::Zeroizing<String>>, CliError> {
    if gateway.auth.method != "bearer-env" {
        return Ok(None);
    }
    credential_store::load(state_dir, gateway_name).map_err(CliError::user)
}

pub(crate) fn reject_credential_command_root(
    root: &std::path::Path,
    state_dir: &std::path::Path,
    flag: &str,
) -> Result<(), CliError> {
    let state = std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let credentials = credential_store::directory_path(&state);
    if credentials.starts_with(root) || root.starts_with(&credentials) {
        return Err(CliError::user(format!(
            "{flag} '{}' intersects Kitsu's managed-credential namespace; use a narrower project/toolchain root",
            terminal_inline(&root.display().to_string())
        )));
    }
    Ok(())
}

pub(crate) fn auth_status_payload(
    gateway_name: &str,
    gateway: &Gateway,
    managed: Option<zeroize::Zeroizing<String>>,
) -> (serde_json::Value, bool) {
    let environment_present = environment_override_present(gateway);
    let managed_present = managed.is_some();
    let effective_source = match gateway.auth.method.as_str() {
        "none" => "none",
        "static" => "static",
        "bearer-env" if environment_present => "environment",
        "bearer-env" if managed_present => "managed",
        "bearer-env" => "missing",
        _ => "unsupported",
    };
    let credential_present = match gateway.auth.method.as_str() {
        "none" => false,
        "static" => gateway
            .auth
            .token
            .as_deref()
            .is_some_and(|token| !token.is_empty()),
        "bearer-env" => environment_present || managed_present,
        _ => false,
    };
    let locally_usable =
        build_auth(gateway, gateway_name, managed).is_ok_and(|auth| auth.header().is_ok());
    (
        serde_json::json!({
            "gateway": gateway_name,
            "method": gateway.auth.method,
            "credential_present": credential_present,
            "effective_source": effective_source,
            "environment_override_present": environment_present,
            "managed_credential_present": managed_present,
            "locally_usable": locally_usable,
            "remote_validity_checked": false,
            "request_sent": false,
        }),
        locally_usable,
    )
}

pub(crate) async fn run_auth(cli: &Cli, json: bool, command: &AuthCommand) -> Result<(), CliError> {
    reject_jsonl_for_management(cli, "kitsu auth status --json")?;
    let config_path = Config::resolve_path(cli.config.as_deref());
    if !config_path.is_file() {
        return Err(CliError::user(format!(
            "config error: no readable config found at {}",
            shorten_home(&config_path)
        )));
    }
    let config =
        Config::load(&config_path).map_err(|error| CliError::user(safe_config_error(&error)))?;
    let (gateway_name, gateway) = config
        .select_gateway(cli.gateway.as_deref())
        .map_err(|error| CliError::user(error.to_string()))?;
    gateway
        .validate_auth()
        .map_err(|error| CliError::user(error.to_string()))?;
    let gateway_name = gateway_name.to_string();

    match command {
        AuthCommand::Login { token_stdin } => {
            if gateway.auth.method != "bearer-env" {
                return Err(CliError::user(format!(
                    "auth login supports bearer-env gateways; '{}' uses {}",
                    terminal_inline(&gateway_name),
                    terminal_inline(&gateway.auth.method)
                )));
            }
            require_persistent_credential_home()?;
            let bytes = if *token_stdin {
                read_secret_stdin()?
            } else {
                read_secret_tty().await?
            };
            let token = credential_store::decode_zeroizing(bytes).map_err(CliError::user)?;
            let validator = EnvBearerAuth {
                env: gateway.auth.env.clone(),
                expires_env: gateway.auth.expires_env.clone(),
                header: gateway.auth.header.clone(),
                scheme: gateway.auth.scheme.clone(),
                refresh_hint: None,
                managed_token: None,
            };
            validator
                .validate_managed_token(token.as_str())
                .map_err(|_| CliError::user("managed credential is not a valid HTTP credential"))?;
            let state_dir = runtime_state_dir(&config_path);
            credential_store::store(&state_dir, &gateway_name, token.as_str())
                .map_err(CliError::user)?;
            let managed = managed_credential(&state_dir, &gateway_name, gateway)?;
            let (mut report, _) = auth_status_payload(&gateway_name, gateway, managed);
            report["stored"] = serde_json::Value::Bool(true);
            if json {
                print_json(&report)?;
            } else {
                println!(
                    "managed credential stored for gateway {}; effective source: {}",
                    terminal_inline(&gateway_name),
                    report["effective_source"].as_str().unwrap_or("unknown")
                );
                println!(
                    "remote validity was not checked; run `kitsu --gateway {} doctor --inference`",
                    terminal_inline(&gateway_name)
                );
            }
            Ok(())
        }
        AuthCommand::Status => {
            let state_dir = runtime_state_dir_read_only(&config_path);
            let managed = managed_credential(&state_dir, &gateway_name, gateway)?;
            let (report, locally_usable) = auth_status_payload(&gateway_name, gateway, managed);
            if json {
                print_json(&report)?;
            } else {
                println!(
                    "gateway {}: {} (source: {}; remote validity not checked)",
                    terminal_inline(&gateway_name),
                    if locally_usable {
                        "credential locally usable"
                    } else {
                        "credential unavailable or malformed"
                    },
                    report["effective_source"].as_str().unwrap_or("unknown")
                );
            }
            if locally_usable {
                Ok(())
            } else {
                Err(CliError::user(
                    "authentication is not locally usable; run `kitsu auth login` or set the configured environment variable",
                ))
            }
        }
        AuthCommand::Logout => {
            if gateway.auth.method != "bearer-env" {
                return Err(CliError::user(format!(
                    "auth logout supports bearer-env gateways; '{}' uses {}",
                    terminal_inline(&gateway_name),
                    terminal_inline(&gateway.auth.method)
                )));
            }
            let state_dir = runtime_state_dir_read_only(&config_path);
            let removed =
                credential_store::remove(&state_dir, &gateway_name).map_err(CliError::user)?;
            let managed = managed_credential(&state_dir, &gateway_name, gateway)?;
            let (mut report, _) = auth_status_payload(&gateway_name, gateway, managed);
            report["removed"] = serde_json::Value::Bool(removed);
            if json {
                print_json(&report)?;
            } else {
                println!(
                    "{} managed credential for gateway {}; effective source: {}",
                    if removed { "removed" } else { "no" },
                    terminal_inline(&gateway_name),
                    report["effective_source"].as_str().unwrap_or("unknown")
                );
                if report["environment_override_present"] == true {
                    println!("the configured environment credential remains effective");
                }
            }
            Ok(())
        }
    }
}

pub(crate) fn shell_quote(value: &str) -> String {
    #[cfg(unix)]
    {
        if !value.is_empty()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':')
            })
        {
            value.to_string()
        } else {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    }
    #[cfg(not(unix))]
    {
        serde_json::to_string(value).unwrap_or_else(|_| "\"<path>\"".to_string())
    }
}

pub(crate) fn resume_argv(config_path: &std::path::Path, id: &str) -> Vec<String> {
    vec![
        "kitsu".to_string(),
        "--config".to_string(),
        config_path.to_string_lossy().into_owned(),
        "chat".to_string(),
        "--resume".to_string(),
        id.to_string(),
    ]
}

pub(crate) fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn runnable_resume_command(directory: &str, argv: &[String]) -> String {
    #[cfg(unix)]
    {
        format!("cd {} && {}", shell_quote(directory), display_argv(argv))
    }
    #[cfg(not(unix))]
    {
        // JSON output carries `cwd` separately for portable process launchers.
        // Keep the human fallback explicit instead of guessing a shell dialect.
        format!("(from {}) {}", shell_quote(directory), display_argv(argv))
    }
}

pub(crate) fn session_error(
    operation: &str,
    config_path: &std::path::Path,
    error: impl std::fmt::Display,
) -> CliError {
    CliError::user(format!(
        "cannot {operation} session in the namespace for config '{}': {error}",
        config_path.display()
    ))
}

pub(crate) fn require_session_namespace(
    sessions_dir: &std::path::Path,
    operation: &str,
    config_path: &std::path::Path,
) -> Result<(), CliError> {
    match std::fs::symlink_metadata(sessions_dir) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(session_error(
            operation,
            config_path,
            "no saved sessions in this config directory",
        )),
        // Let SessionStore produce its descriptor/file-type-specific error for
        // permission failures, symlinks, and irregular existing entries.
        Err(_) => Ok(()),
    }
}

pub(crate) fn role_label(role: kitsu_providers::Role) -> &'static str {
    match role {
        kitsu_providers::Role::System => "system",
        kitsu_providers::Role::User => "user",
        kitsu_providers::Role::Assistant => "assistant",
        kitsu_providers::Role::Tool => "tool",
    }
}

pub(crate) fn format_saved_session(saved: &session::SavedSession) -> String {
    const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;
    let mut output = format!(
        "session {}\nupdated: {}\ngateway/model: {}/{}\ndirectory: {}\nmessages: {}\n\n",
        terminal_inline(&saved.id),
        terminal_inline(&saved.updated_at),
        terminal_inline(&saved.gateway),
        terminal_inline(&saved.model),
        shorten_home(std::path::Path::new(&saved.directory)),
        saved.messages.len(),
    );
    let mut truncated = false;
    for message in &saved.messages {
        let content = kitsu_render::sanitize_terminal_text(&message.content);
        let block = format!("[{}]\n{}\n\n", role_label(message.role), content);
        let remaining = MAX_TRANSCRIPT_BYTES.saturating_sub(output.len());
        if block.len() <= remaining {
            output.push_str(&block);
            continue;
        }
        let mut boundary = remaining.min(block.len());
        while boundary > 0 && !block.is_char_boundary(boundary) {
            boundary -= 1;
        }
        output.push_str(&block[..boundary]);
        truncated = true;
        break;
    }
    if truncated {
        output.push_str(
            "\n\n[transcript truncated at 64 KiB; use --json or export for the complete snapshot]\n",
        );
    }
    output
}

pub(crate) fn artifact_generation_payload(generation: &artifacts::Generation) -> serde_json::Value {
    serde_json::json!({
        "id": generation.id,
        "path": generation.path,
        "namespace": generation.namespace.label(),
        "files": generation.files,
        "bytes": generation.bytes,
        "age_secs": generation.age_secs,
        "state": generation.retention.label(),
        "prunable": generation.prunable(),
    })
}

pub(crate) fn format_artifact_inventory(
    generations: &[artifacts::Generation],
    config_path: &std::path::Path,
) -> String {
    let mut out = format!("command artifacts for config {}", shorten_home(config_path));
    for namespace in [
        artifacts::Namespace::Runtime,
        artifacts::Namespace::Sessions,
    ] {
        let group = generations
            .iter()
            .filter(|generation| generation.namespace == namespace)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("\n  namespace {}", namespace.label()));
        for generation in group {
            out.push_str(&format!(
                "\n    {:<40} {:>3} file(s) {:>10}  {:<9} {}",
                terminal_inline(&generation.id),
                generation.files,
                artifacts::human_bytes(generation.bytes),
                artifacts::human_age(generation.age_secs),
                generation.retention.label(),
            ));
        }
    }
    let total_bytes: u64 = generations.iter().map(|generation| generation.bytes).sum();
    let prunable = generations
        .iter()
        .filter(|generation| generation.prunable())
        .count();
    let live = generations
        .iter()
        .filter(|generation| generation.retention == artifacts::Retention::InUse)
        .count();
    out.push_str(&format!(
        "\n  total {} generation(s), {}; {prunable} prunable, {live} in use by a live process",
        generations.len(),
        artifacts::human_bytes(total_bytes),
    ));
    out.push_str(
        "\n  prune with: kitsu artifacts prune all | kitsu artifacts prune all --older-than 7d",
    );
    out
}

/// `kitsu artifacts` — provider-free, offline, host-only inventory and pruning
/// of storage left by earlier runs. Listing is strictly read-only: merely
/// looking at disk state must not materialize a profile directory.
pub(crate) fn run_artifacts(
    cli: &Cli,
    json: bool,
    command: Option<&cli::ArtifactCommand>,
) -> Result<(), CliError> {
    reject_jsonl_for_management(cli, "kitsu artifacts --json")?;
    let config_path = Config::resolve_path(cli.config.as_deref());
    let state_dir = runtime_state_dir_read_only(&config_path);
    let generations = artifacts::scan(&state_dir).map_err(CliError::user)?;

    match command {
        None | Some(cli::ArtifactCommand::List) => {
            if json {
                return print_json(&serde_json::json!({
                    "action": "list",
                    "state_dir": state_dir,
                    "generations": generations
                        .iter()
                        .map(artifact_generation_payload)
                        .collect::<Vec<_>>(),
                    "total_bytes": generations.iter().map(|generation| generation.bytes).sum::<u64>(),
                }));
            }
            if generations.is_empty() {
                println!(
                    "no retained command artifacts for config {}",
                    shorten_home(&config_path)
                );
                return Ok(());
            }
            println!("{}", format_artifact_inventory(&generations, &config_path));
            Ok(())
        }
        Some(cli::ArtifactCommand::Prune {
            target,
            older_than,
            dry_run,
        }) => {
            let minimum_age = older_than
                .as_deref()
                .map(artifacts::parse_duration_secs)
                .transpose()
                .map_err(CliError::user)?;
            let selected = generations
                .iter()
                .filter(|generation| target == "all" || &generation.id == target)
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(CliError::user(format!(
                    "no retained artifact generation matches {}; run 'kitsu artifacts list' to see what exists",
                    terminal_inline(target)
                )));
            }
            let mut removed = Vec::new();
            let mut skipped = Vec::new();
            let mut reclaimed = 0u64;
            for generation in selected {
                if !generation.prunable() {
                    skipped.push((generation, skip_reason(generation)));
                    continue;
                }
                if minimum_age.is_some_and(|minimum| {
                    generation.age_secs.map(|age| age < minimum).unwrap_or(true)
                }) {
                    skipped.push((generation, "newer than --older-than".to_string()));
                    continue;
                }
                if *dry_run {
                    reclaimed += generation.bytes;
                    removed.push(generation);
                    continue;
                }
                match artifacts::remove(generation) {
                    Ok(()) => {
                        reclaimed += generation.bytes;
                        removed.push(generation);
                    }
                    Err(error) => skipped.push((generation, error)),
                }
            }
            if json {
                return print_json(&serde_json::json!({
                    "action": if *dry_run { "prune_dry_run" } else { "prune" },
                    "removed": removed
                        .iter()
                        .map(|generation| artifact_generation_payload(generation))
                        .collect::<Vec<_>>(),
                    "skipped": skipped
                        .iter()
                        .map(|(generation, reason)| serde_json::json!({
                            "id": generation.id,
                            "reason": reason,
                        }))
                        .collect::<Vec<_>>(),
                    "reclaimed_bytes": reclaimed,
                }));
            }
            println!(
                "{} {} generation(s), {}",
                if *dry_run { "would remove" } else { "removed" },
                removed.len(),
                artifacts::human_bytes(reclaimed)
            );
            for (generation, reason) in &skipped {
                println!("  skipped {}: {reason}", terminal_inline(&generation.id));
            }
            if removed.is_empty() {
                return Err(CliError::user(
                    "nothing was removed; every matching generation is still in use or owned"
                        .to_string(),
                ));
            }
            Ok(())
        }
    }
}

pub(crate) fn skip_reason(generation: &artifacts::Generation) -> String {
    match generation.retention {
        artifacts::Retention::InUse => "a live process holds this generation's lock".to_string(),
        artifacts::Retention::SessionExists => {
            "its session still exists; use 'kitsu sessions delete <id>' first".to_string()
        }
        artifacts::Retention::Orphaned => format!(
            "no lock file and younger than the {}s liveness grace period",
            artifacts::UNLOCKED_GRACE_SECS
        ),
    }
}

/// `kitsu agents`: the background agents every kitsu on this machine is
/// publishing. Host-only and read-only — it opens no provider, makes no
/// inference request, needs no config, and reclaims no state file.
pub(crate) fn run_agents(cli: &Cli, json: bool, all: bool) -> Result<(), CliError> {
    reject_jsonl_for_management(cli, "kitsu agents --json")?;
    let entries = agent_state_file::list_entries(
        &agent_state_file::state_dir(),
        all,
        &agent_state_file::process_is_alive,
    );
    if json {
        // A listing like `kitsu sessions --json`: the bare array, with no
        // action envelope.
        print_json(&entries)
    } else {
        println!("{}", agent_state_file::format_agents_listing(&entries));
        Ok(())
    }
}

pub(crate) async fn run_sessions(
    cli: &Cli,
    json: bool,
    command: Option<&SessionCommand>,
) -> Result<(), CliError> {
    reject_jsonl_for_management(cli, "kitsu sessions --json")?;
    let config_path = Config::resolve_path(cli.config.as_deref());
    let state_dir = runtime_state_dir_read_only(&config_path);
    let sessions_dir = state_dir.join("sessions");

    match command {
        None => {
            let saved = session::SessionStore::list(&sessions_dir)
                .map_err(|error| session_error("list", &config_path, error))?;
            if json {
                print_json(&saved)
            } else if saved.is_empty() {
                println!(
                    "no saved sessions for config {} — start one with kitsu chat --session (add --name <name> for a memorable handle) or /new",
                    shorten_home(&config_path)
                );
                Ok(())
            } else {
                println!("{}", format_session_summaries(&saved, None, ""));
                Ok(())
            }
        }
        Some(SessionCommand::List { filter }) => {
            let saved = session::SessionStore::list(&sessions_dir)
                .map_err(|error| session_error("list", &config_path, error))?;
            let filter_str = filter.as_deref().unwrap_or("");
            if json {
                let filtered = filter_sessions(&saved, filter_str);
                print_json(&filtered)
            } else if saved.is_empty() {
                println!(
                    "no saved sessions for config {} — start one with kitsu chat --session (add --name <name> for a memorable handle) or /new",
                    shorten_home(&config_path)
                );
                Ok(())
            } else {
                println!("{}", format_session_summaries(&saved, None, filter_str));
                Ok(())
            }
        }
        Some(SessionCommand::Show { id }) => {
            require_session_namespace(&sessions_dir, "show", &config_path)?;
            let saved = session::SessionStore::load(&sessions_dir, id)
                .map_err(|error| session_error("show", &config_path, error))?;
            if json {
                print_json(&saved)
            } else {
                print!("{}", format_saved_session(&saved));
                Ok(())
            }
        }
        Some(SessionCommand::Resume { id }) => {
            require_session_namespace(&sessions_dir, "resume", &config_path)?;
            let saved = session::SessionStore::load(&sessions_dir, id)
                .map_err(|error| session_error("resume", &config_path, error))?;
            if json || !kitsu_render::stdin_is_tty() {
                let argv = resume_argv(&config_path, &saved.id);
                if json {
                    print_json(&serde_json::json!({
                        "action": "resume",
                        "launched": false,
                        "session_id": saved.id,
                        "cwd": saved.directory,
                        "argv": argv,
                    }))
                } else {
                    println!(
                        "resume with: {}",
                        terminal_inline(&runnable_resume_command(&saved.directory, &argv))
                    );
                    println!("working directory: {}", terminal_inline(&saved.directory));
                    println!(
                        "exact argv: {}",
                        serde_json::to_string(&argv)
                            .unwrap_or_else(|_| "[\"kitsu\", \"chat\"]".to_string())
                    );
                    Ok(())
                }
            } else {
                run_chat(
                    cli,
                    ChatInvocation {
                        first_prompt: None,
                        no_tools: false,
                        allow_paths: &[],
                        allow_write: &[],
                        allow_read_write: &[],
                        require_writes: &[],
                        require_write_sha256: &[],
                        enable_commands: false,
                        command_roots: &[],
                        command_runtime_roots: &[],
                        command_env: &[],
                        allow_commands: &[],
                        allow_network: false,
                        script: None,
                        resume: Some(&saved.id),
                        persist_session: false,
                        no_session: false,
                        session_name: None,
                        allow_turn_errors: false,
                        initial_review: None,
                    },
                )
                .await
            }
        }
        Some(SessionCommand::Fork { id }) => {
            require_session_namespace(&sessions_dir, "fork", &config_path)?;
            let forked = session::SessionStore::fork_with_source(&sessions_dir, id)
                .map_err(|error| session_error("fork", &config_path, error))?;
            if json {
                print_json(&serde_json::json!({
                    "action": "fork",
                    "source_session_id": forked.source_session_id,
                    "session": forked.session,
                    "permissions_restored": false,
                }))
            } else {
                println!(
                    "forked session {} -> {} (permissions were not copied)",
                    terminal_inline(&forked.source_session_id),
                    terminal_inline(&forked.session.id)
                );
                Ok(())
            }
        }
        Some(SessionCommand::Export {
            id,
            destination,
            format,
        }) => {
            require_session_namespace(&sessions_dir, "export", &config_path)?;
            let format = match format.as_deref() {
                Some("markdown") => session::SessionExportFormat::Markdown,
                Some("json") => session::SessionExportFormat::Json,
                Some(_) => unreachable!("clap validates session export format"),
                // An explicit --format always wins; inference only fills the gap
                // that used to silently write Markdown into a .json file.
                None => {
                    let inferred = inferred_export_format(destination);
                    if inferred == session::SessionExportFormat::Json && !json {
                        eprintln!("~ inferred --format json from the .json destination");
                    }
                    inferred
                }
            };
            let output = session::SessionStore::export(&sessions_dir, id, destination, format)
                .map_err(|error| session_error("export", &config_path, error))?;
            if json {
                print_json(&serde_json::json!({"action": "export", "path": output}))
            } else {
                println!("exported session to {}", output.display());
                Ok(())
            }
        }
        Some(SessionCommand::Rename { id, name }) => {
            require_session_namespace(&sessions_dir, "rename", &config_path)?;
            let saved = session::SessionStore::rename(&sessions_dir, id, name.as_deref())
                .map_err(|error| session_error("rename", &config_path, error))?;
            if json {
                print_json(&serde_json::json!({
                    "action": "rename",
                    "id": saved.id,
                    "name": saved.name,
                }))
            } else {
                match &saved.name {
                    Some(new_name) => println!(
                        "named session {} '{}' — resume it with `kitsu chat --resume {}`",
                        terminal_inline(&saved.id),
                        terminal_inline(new_name),
                        terminal_inline(new_name),
                    ),
                    None => println!("cleared the name of session {}", terminal_inline(&saved.id)),
                }
                Ok(())
            }
        }
        Some(SessionCommand::Archive { id }) => {
            require_session_namespace(&sessions_dir, "archive", &config_path)?;
            session::SessionStore::load(&sessions_dir, id)
                .map_err(|error| session_error("archive", &config_path, error))?;
            let output = session::SessionStore::archive(&sessions_dir, id, None)
                .map_err(|error| session_error("archive", &config_path, error))?;
            if json {
                print_json(&serde_json::json!({"action": "archive", "id": id, "path": output}))
            } else {
                println!("archived session {}", terminal_inline(id));
                Ok(())
            }
        }
        Some(SessionCommand::Restore { id }) => {
            require_session_namespace(&sessions_dir, "restore", &config_path)?;
            let saved = session::SessionStore::restore_archived(&sessions_dir, id)
                .map_err(|error| session_error("restore", &config_path, error))?;
            if json {
                print_json(&serde_json::json!({"action": "restore", "session": saved}))
            } else {
                println!("restored session {}", terminal_inline(&saved.id));
                Ok(())
            }
        }
        Some(SessionCommand::Delete { id }) => {
            require_session_namespace(&sessions_dir, "delete", &config_path)?;
            session::SessionStore::load(&sessions_dir, id)
                .map_err(|error| session_error("delete", &config_path, error))?;
            session::SessionStore::delete(&sessions_dir, id, None)
                .map_err(|error| session_error("delete", &config_path, error))?;
            if json {
                print_json(&serde_json::json!({"action": "delete", "id": id, "deleted": true}))
            } else {
                println!("deleted session {}", terminal_inline(id));
                Ok(())
            }
        }
    }
}

pub(crate) fn config_resolution_source(cli: &Cli) -> &'static str {
    if cli.config.is_some() {
        "--config"
    } else if std::env::var_os("KITSU_CONFIG").is_some_and(|value| !value.is_empty()) {
        "KITSU_CONFIG"
    } else {
        "default"
    }
}

#[cfg(unix)]
pub(crate) fn directory_is_writable(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

#[cfg(not(unix))]
pub(crate) fn directory_is_writable(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_dir() && !metadata.permissions().readonly())
        .unwrap_or(false)
}

pub(crate) fn runtime_state_diagnostic(path: &std::path::Path) -> serde_json::Value {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => serde_json::json!({
            "path": path,
            "exists": true,
            "writable": false,
            "status": "unsafe: state path is a symlink",
        }),
        Ok(metadata) if !metadata.file_type().is_dir() => serde_json::json!({
            "path": path,
            "exists": true,
            "writable": false,
            "status": "unusable: state path is not a directory",
        }),
        Ok(_) => {
            let writable = directory_is_writable(path);
            serde_json::json!({
                "path": path,
                "exists": true,
                "writable": writable,
                "status": if writable { "writable" } else { "not writable" },
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut ancestor = path.parent();
            let mut nearest = None;
            while let Some(candidate) = ancestor {
                match std::fs::canonicalize(candidate) {
                    Ok(existing) if existing.is_dir() => {
                        nearest = Some(existing);
                        break;
                    }
                    _ => ancestor = candidate.parent(),
                }
            }
            let writable = nearest
                .as_deref()
                .map(directory_is_writable)
                .unwrap_or(false);
            serde_json::json!({
                "path": path,
                "exists": false,
                "writable": writable,
                "nearest_existing_ancestor": nearest,
                "status": if writable {
                    "not created; nearest parent is writable"
                } else {
                    "not created; nearest parent is not writable"
                },
            })
        }
        Err(error) => serde_json::json!({
            "path": path,
            "exists": false,
            "writable": false,
            "status": format!("cannot inspect state path: {}", error.kind()),
        }),
    }
}

pub(crate) fn safe_config_error(error: &ConfigError) -> String {
    match error {
        ConfigError::Parse { .. } => error.to_string(),
        ConfigError::Read { source, .. } => {
            format!("cannot read config: {}", source.kind())
        }
        other => other.to_string(),
    }
}
