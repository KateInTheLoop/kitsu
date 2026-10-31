//! Command grants, environment policy, and session resets.

use kitsu_tools::Mode;
use std::sync::Arc;

use crate::cli_error::CliError;

pub(crate) fn dangerous_bypass_revocation_error(mode: Mode, access: &str) -> Option<String> {
    (mode == Mode::AutoAllow).then(|| {
        format!(
            "cannot revoke {access} roots while --dangerously-allow-all-filesystem is active: \
             the global bypass would still authorize every non-protected path; restart without \
             the dangerous flag to use scoped revocation"
        )
    })
}

/// The wire shape of one `--allow-command` value. The bare array stays
/// byte-compatible with every existing script; the object form names the
/// working directory the grant is bound to, so automation launched outside a
/// `--command-root` no longer has to `cd` to make a pregrant satisfiable.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(crate) enum PregrantedCommandSpec {
    Argv(Vec<String>),
    Explicit {
        argv: Vec<String>,
        cwd: std::path::PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CwdSource {
    LaunchDirectory,
    Explicit,
}

impl CwdSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            CwdSource::LaunchDirectory => "launch directory",
            CwdSource::Explicit => "explicit cwd",
        }
    }

    pub(crate) fn payload_value(self) -> &'static str {
        match self {
            CwdSource::LaunchDirectory => "launch_directory",
            CwdSource::Explicit => "explicit",
        }
    }
}

/// One resolved pregrant: exact argv bound to exactly one working directory.
/// `cwd` is rewritten to the policy's canonical form once the grant lands, so
/// `/new` re-grants and `/permissions` provenance both key off the same path.
#[derive(Debug, Clone)]
pub(crate) struct PregrantedCommand {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) cwd_source: CwdSource,
}

impl PregrantedCommandSpec {
    pub(crate) fn parse(encoded: &str) -> Result<Self, CliError> {
        serde_json::from_str(encoded).map_err(|error| {
            CliError::user(format!(
                "invalid --allow-command value {encoded:?}: expected a JSON string array such as \
                 '[\"cargo\",\"test\"]', or an object such as \
                 '{{\"argv\":[\"cargo\",\"test\"],\"cwd\":\"/path/inside/a/command/root\"}}' ({error})"
            ))
        })
    }

    /// Bind the spec to a working directory. A relative explicit cwd is refused
    /// rather than resolved against the process directory: that would reintroduce
    /// exactly the ambiguity the object form exists to remove.
    pub(crate) fn resolve(
        &self,
        launch_dir: &std::path::Path,
    ) -> Result<PregrantedCommand, CliError> {
        let (argv, cwd, cwd_source) = match self {
            PregrantedCommandSpec::Argv(argv) => (
                argv.clone(),
                launch_dir.to_path_buf(),
                CwdSource::LaunchDirectory,
            ),
            PregrantedCommandSpec::Explicit { argv, cwd } => {
                if !cwd.is_absolute() {
                    return Err(CliError::user(format!(
                        "invalid --allow-command {}: \"cwd\" must be an absolute path, not {}",
                        display_pregrant_argv(argv),
                        cwd.display()
                    )));
                }
                (argv.clone(), cwd.clone(), CwdSource::Explicit)
            }
        };
        if argv.is_empty() {
            return Err(CliError::user(
                "invalid --allow-command: \"argv\" must name at least one program".to_string(),
            ));
        }
        Ok(PregrantedCommand {
            argv,
            cwd,
            cwd_source,
        })
    }
}

pub(crate) fn display_pregrant_argv(argv: &[String]) -> String {
    serde_json::to_string(argv).unwrap_or_else(|_| "[invalid argv]".to_string())
}

/// The wire shape of one `--command-env` value. A bare `NAME` reads the
/// value from kitsu's own process environment (subject to the secret-name
/// policy on [`Self::resolve`]); `NAME=value` supplies an explicit literal
/// the caller typed directly, always allowed regardless of name shape.
#[derive(Debug, Clone)]
pub(crate) enum CommandEnvSpec {
    Passthrough(String),
    Explicit(String, String),
}

impl CommandEnvSpec {
    pub(crate) fn parse(raw: &str) -> Result<Self, CliError> {
        if let Some((name, value)) = raw.split_once('=') {
            let name = name.trim();
            if name.is_empty() {
                return Err(CliError::user(format!(
                    "invalid --command-env {raw:?}: name before '=' must not be empty"
                )));
            }
            return Ok(CommandEnvSpec::Explicit(
                name.to_string(),
                value.to_string(),
            ));
        }
        let name = raw.trim();
        if name.is_empty() {
            return Err(CliError::user(
                "invalid --command-env: expected NAME or NAME=value".to_string(),
            ));
        }
        Ok(CommandEnvSpec::Passthrough(name.to_string()))
    }

    /// Resolve to a concrete (name, value) pair. A bare pass-through of a
    /// name that looks secret-bearing is refused outright: deny-by-default
    /// means nothing is silently forwarded from kitsu's own environment
    /// unless the caller can name it AND (for secret-shaped names) type the
    /// literal value explicitly.
    pub(crate) fn resolve(&self) -> Result<(String, String), CliError> {
        match self {
            CommandEnvSpec::Explicit(name, value) => Ok((name.clone(), value.clone())),
            CommandEnvSpec::Passthrough(name) => {
                if kitsu_tools::env_name_looks_secret(name) {
                    return Err(CliError::user(format!(
                        "refusing --command-env {name}: the name looks secret-bearing (matches \
                         a case-insensitive KEY/TOKEN/SECRET/PASSWORD substring), so it cannot be \
                         silently passed through by name; use --command-env {name}=<value> to \
                         pass an explicit value instead"
                    )));
                }
                let value = std::env::var(name).map_err(|_| {
                    CliError::user(format!(
                        "invalid --command-env {name:?}: not set in kitsu's own environment; use \
                         --command-env {name}=<value> to pass an explicit value instead"
                    ))
                })?;
                Ok((name.clone(), value))
            }
        }
    }
}

/// Startup failure text for one pregrant. When the legacy array form failed only
/// because the launch directory sits outside the approved roots, name the object
/// syntax that makes the same grant expressible.
pub(crate) fn pregrant_error(grant: &PregrantedCommand, roots: &[String], error: &str) -> String {
    let base = format!(
        "invalid --allow-command {}: {error}",
        display_pregrant_argv(&grant.argv)
    );
    if grant.cwd_source == CwdSource::Explicit || !error.contains("outside the approved command") {
        return base;
    }
    let suggestion = roots
        .first()
        .cloned()
        .unwrap_or_else(|| "/path".to_string());
    format!(
        "{base}; the launch directory {} is outside the approved command roots ({}) — pass \
         {{\"argv\":{},\"cwd\":\"{suggestion}\"}} to bind the grant to a root explicitly",
        grant.cwd.display(),
        roots.join(", "),
        display_pregrant_argv(&grant.argv),
    )
}

pub(crate) fn reset_command_session(
    policy: Option<&Arc<kitsu_tools::CommandPolicy>>,
    explicit_grants: &[PregrantedCommand],
) {
    let Some(policy) = policy else {
        return;
    };
    policy.reset_session_grants();
    // Auto mode is session state too: /new and /resume start a fresh session,
    // so a prompt-chosen bypass must not silently carry into it.
    policy.set_session_auto(false);
    for grant in explicit_grants {
        // Re-grant against the cwd the pregrant was bound to at startup, never
        // the current launch directory: an explicit-cwd grant must survive /new
        // and /resume unchanged rather than being silently rebound.
        if let Err(error) = policy.grant_command(&grant.argv, &grant.cwd) {
            eprintln!(
                "warning: could not restore explicit command grant {} (cwd {}): {error}",
                display_pregrant_argv(&grant.argv),
                grant.cwd.display()
            );
        }
    }
}

/// The live command-mode label for `/status`, `/permissions`, and the JSON
/// receipts, computed from the policy so it reflects a prompt-chosen auto mode
/// rather than the mode fixed at startup. `None` when commands are disabled.
pub(crate) fn command_mode_label(
    policy: Option<&Arc<kitsu_tools::CommandPolicy>>,
) -> Option<&'static str> {
    policy.map(|policy| {
        if policy.session_auto() {
            return "auto (no prompts this session; /auto off to restore asking)";
        }
        match policy.mode() {
            kitsu_tools::CommandMode::Ask => "ask",
            kitsu_tools::CommandMode::Deny => "deny unless pre-granted",
            kitsu_tools::CommandMode::AutoAllow => "DANGEROUS auto-allow (no prompts)",
        }
    })
}
