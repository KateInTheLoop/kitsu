//! Provider-free admission values for a future bounded child supervisor.
//!
//! Admission is deliberately split from executable observation. Disabled,
//! denied, unsupported, malformed, or authority-mismatched requests return
//! before the injected preparation boundary can canonicalize or hash a path
//! or inspect the containment backend. No process API exists in this module.

#![allow(dead_code)]

use std::path::{Component, Path, PathBuf};

const CHILD_LAUNCH_REQUEST_VERSION_V1: u32 = 1;
const SHA256_HEX_BYTES: usize = 64;
const MAX_ARGUMENT_COUNT: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_TOTAL_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildLaunchModeV1 {
    Disabled,
    Enabled,
}

/// Inert, untrusted launch facts. Constructing this value performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildLaunchRequestV1 {
    version: u32,
    mode: ChildLaunchModeV1,
    executable: PathBuf,
    executable_sha256: String,
    arguments: Vec<String>,
    working_directory: PathBuf,
}

impl ChildLaunchRequestV1 {
    pub(crate) fn new(
        version: u32,
        mode: ChildLaunchModeV1,
        executable: impl Into<PathBuf>,
        executable_sha256: impl Into<String>,
        arguments: Vec<String>,
        working_directory: impl Into<PathBuf>,
    ) -> Self {
        Self {
            version,
            mode,
            executable: executable.into(),
            executable_sha256: executable_sha256.into(),
            arguments,
            working_directory: working_directory.into(),
        }
    }
}

/// Host-authored authority, kept separate from untrusted request facts.
///
/// V1 is intentionally exact rather than root- or pattern-based: executable,
/// content digest, argv, and cwd must all match. Future authority expansion is
/// a separate reviewed change, not a broader interpretation of these fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityProfileV1 {
    spec: ValidatedChildLaunchSpecV1,
}

impl AuthorityProfileV1 {
    pub(crate) fn exact(
        executable: impl Into<PathBuf>,
        executable_sha256: impl Into<String>,
        arguments: Vec<String>,
        working_directory: impl Into<PathBuf>,
    ) -> Result<Self, ChildLaunchAdmissionErrorV1> {
        Ok(Self {
            spec: ValidatedChildLaunchSpecV1::new(
                executable.into(),
                executable_sha256.into(),
                arguments,
                working_directory.into(),
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildLaunchAuthorityDecisionV1<'a> {
    Deny,
    Admit(&'a AuthorityProfileV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorityMismatchFieldV1 {
    Executable,
    ExecutableSha256,
    Arguments,
    WorkingDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChildLaunchAdmissionErrorV1 {
    Disabled,
    AuthorityDenied,
    UnsupportedVersion { observed: u32 },
    InvalidExecutablePath,
    InvalidExecutableSha256,
    InvalidArguments,
    InvalidWorkingDirectory,
    AuthorityMismatch(AuthorityMismatchFieldV1),
    ExecutableCanonicalization(String),
    ExecutablePathChanged,
    ExecutableHash(String),
    ExecutableDigestMismatch,
    ContainmentPreflight(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutableSha256V1([u8; 32]);

impl ExecutableSha256V1 {
    fn parse(value: &str) -> Result<Self, ChildLaunchAdmissionErrorV1> {
        if value.len() != SHA256_HEX_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ChildLaunchAdmissionErrorV1::InvalidExecutableSha256);
        }

        let mut digest = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            digest[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
        }
        Ok(Self(digest))
    }
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("digest syntax is checked before decoding"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedChildLaunchSpecV1 {
    executable: PathBuf,
    executable_sha256: ExecutableSha256V1,
    arguments: Vec<String>,
    working_directory: PathBuf,
}

impl ValidatedChildLaunchSpecV1 {
    fn new(
        executable: PathBuf,
        executable_sha256: String,
        arguments: Vec<String>,
        working_directory: PathBuf,
    ) -> Result<Self, ChildLaunchAdmissionErrorV1> {
        if !is_normalized_absolute_path(&executable) {
            return Err(ChildLaunchAdmissionErrorV1::InvalidExecutablePath);
        }
        let executable_sha256 = ExecutableSha256V1::parse(&executable_sha256)?;
        validate_arguments(&arguments)?;
        if !is_normalized_absolute_path(&working_directory) {
            return Err(ChildLaunchAdmissionErrorV1::InvalidWorkingDirectory);
        }
        Ok(Self {
            executable,
            executable_sha256,
            arguments,
            working_directory,
        })
    }
}

fn is_normalized_absolute_path(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    path.is_absolute()
        && !text.contains('\0')
        && path.components().all(|component| match component {
            Component::Prefix(_) => cfg!(windows),
            Component::RootDir | Component::Normal(_) => true,
            Component::CurDir | Component::ParentDir => false,
        })
}

fn validate_arguments(arguments: &[String]) -> Result<(), ChildLaunchAdmissionErrorV1> {
    if arguments.len() > MAX_ARGUMENT_COUNT {
        return Err(ChildLaunchAdmissionErrorV1::InvalidArguments);
    }
    let mut total = 0_usize;
    for argument in arguments {
        if argument.as_bytes().contains(&0) || argument.len() > MAX_ARGUMENT_BYTES {
            return Err(ChildLaunchAdmissionErrorV1::InvalidArguments);
        }
        total = total.saturating_add(argument.len());
        if total > MAX_TOTAL_ARGUMENT_BYTES {
            return Err(ChildLaunchAdmissionErrorV1::InvalidArguments);
        }
    }
    Ok(())
}

/// Pure admission result. Possessing it proves only that inert request facts
/// matched host policy; it does not prove that an executable exists or that
/// containment is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdmittedChildLaunchV1 {
    spec: ValidatedChildLaunchSpecV1,
}

/// Preparation operations that may observe the host after pure admission.
///
/// The eventual supervisor will provide the production implementation. This
/// card injects the boundary so tests can prove rejected inputs invoke none of
/// these operations. There is deliberately no spawn method.
pub(crate) trait ChildLaunchPreparationV1 {
    fn canonicalize_executable(&mut self, executable: &Path) -> Result<PathBuf, String>;
    fn executable_sha256(&mut self, executable: &Path) -> Result<[u8; 32], String>;
    fn containment_preflight(&mut self, launch: &AdmittedChildLaunchV1) -> Result<(), String>;
}

/// Prepared, still-unspawned identity for the future lifecycle owner.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PreparedChildLaunchV1 {
    canonical_executable: PathBuf,
    executable_sha256: ExecutableSha256V1,
    arguments: Vec<String>,
    working_directory: PathBuf,
}

pub(crate) fn admit_child_launch_v1(
    request: ChildLaunchRequestV1,
    decision: ChildLaunchAuthorityDecisionV1<'_>,
) -> Result<AdmittedChildLaunchV1, ChildLaunchAdmissionErrorV1> {
    // Disabled and denied modes intentionally precede version and syntax
    // inspection. An inert entry cannot cause executable discovery merely by
    // containing malformed or future-version fields.
    if request.mode == ChildLaunchModeV1::Disabled {
        return Err(ChildLaunchAdmissionErrorV1::Disabled);
    }
    let profile = match decision {
        ChildLaunchAuthorityDecisionV1::Deny => {
            return Err(ChildLaunchAdmissionErrorV1::AuthorityDenied)
        }
        ChildLaunchAuthorityDecisionV1::Admit(profile) => profile,
    };
    if request.version != CHILD_LAUNCH_REQUEST_VERSION_V1 {
        return Err(ChildLaunchAdmissionErrorV1::UnsupportedVersion {
            observed: request.version,
        });
    }
    let requested = ValidatedChildLaunchSpecV1::new(
        request.executable,
        request.executable_sha256,
        request.arguments,
        request.working_directory,
    )?;
    if requested.executable != profile.spec.executable {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::Executable,
        ));
    }
    if requested.executable_sha256 != profile.spec.executable_sha256 {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::ExecutableSha256,
        ));
    }
    if requested.arguments != profile.spec.arguments {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::Arguments,
        ));
    }
    if requested.working_directory != profile.spec.working_directory {
        return Err(ChildLaunchAdmissionErrorV1::AuthorityMismatch(
            AuthorityMismatchFieldV1::WorkingDirectory,
        ));
    }
    Ok(AdmittedChildLaunchV1 { spec: requested })
}

pub(crate) fn prepare_admitted_child_launch_v1<P>(
    admitted: AdmittedChildLaunchV1,
    preparation: &mut P,
) -> Result<PreparedChildLaunchV1, ChildLaunchAdmissionErrorV1>
where
    P: ChildLaunchPreparationV1,
{
    let canonical_executable = preparation
        .canonicalize_executable(&admitted.spec.executable)
        .map_err(ChildLaunchAdmissionErrorV1::ExecutableCanonicalization)?;
    if canonical_executable != admitted.spec.executable {
        return Err(ChildLaunchAdmissionErrorV1::ExecutablePathChanged);
    }
    let observed_sha256 = preparation
        .executable_sha256(&canonical_executable)
        .map_err(ChildLaunchAdmissionErrorV1::ExecutableHash)?;
    if observed_sha256 != admitted.spec.executable_sha256.0 {
        return Err(ChildLaunchAdmissionErrorV1::ExecutableDigestMismatch);
    }
    preparation
        .containment_preflight(&admitted)
        .map_err(ChildLaunchAdmissionErrorV1::ContainmentPreflight)?;

    Ok(PreparedChildLaunchV1 {
        canonical_executable,
        executable_sha256: admitted.spec.executable_sha256,
        arguments: admitted.spec.arguments,
        working_directory: admitted.spec.working_directory,
    })
}

pub(crate) fn prepare_child_launch_request_v1<P>(
    request: ChildLaunchRequestV1,
    decision: ChildLaunchAuthorityDecisionV1<'_>,
    preparation: &mut P,
) -> Result<PreparedChildLaunchV1, ChildLaunchAdmissionErrorV1>
where
    P: ChildLaunchPreparationV1,
{
    let admitted = admit_child_launch_v1(request, decision)?;
    prepare_admitted_child_launch_v1(admitted, preparation)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXECUTABLE: &str = "/opt/kitsu-fixtures/stdio-peer";
    const WORKING_DIRECTORY: &str = "/tmp/kitsu-external";
    const DIGEST: &str = "abababababababababababababababababababababababababababababababab";

    fn digest_bytes() -> [u8; 32] {
        [0xab; 32]
    }

    fn valid_request() -> ChildLaunchRequestV1 {
        ChildLaunchRequestV1::new(
            CHILD_LAUNCH_REQUEST_VERSION_V1,
            ChildLaunchModeV1::Enabled,
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
        )
    }

    fn profile() -> AuthorityProfileV1 {
        AuthorityProfileV1::exact(
            EXECUTABLE,
            DIGEST,
            vec!["--stdio".to_string()],
            WORKING_DIRECTORY,
        )
        .unwrap()
    }

    #[derive(Default)]
    struct EffectProbe {
        canonicalizations: usize,
        hashes: usize,
        preflights: usize,
        spawns: usize,
        canonical_path: Option<PathBuf>,
        observed_digest: Option<[u8; 32]>,
        canonicalization_error: Option<String>,
        hash_error: Option<String>,
        preflight_error: Option<String>,
    }

    impl EffectProbe {
        fn successful() -> Self {
            Self {
                canonical_path: Some(PathBuf::from(EXECUTABLE)),
                observed_digest: Some(digest_bytes()),
                ..Self::default()
            }
        }

        fn assert_zero_effects(&self) {
            assert_eq!(self.canonicalizations, 0);
            assert_eq!(self.hashes, 0);
            assert_eq!(self.preflights, 0);
            assert_eq!(self.spawns, 0);
        }
    }

    impl ChildLaunchPreparationV1 for EffectProbe {
        fn canonicalize_executable(&mut self, _: &Path) -> Result<PathBuf, String> {
            self.canonicalizations += 1;
            match self.canonicalization_error.take() {
                Some(error) => Err(error),
                None => Ok(self
                    .canonical_path
                    .clone()
                    .expect("test canonical path is configured")),
            }
        }

        fn executable_sha256(&mut self, _: &Path) -> Result<[u8; 32], String> {
            self.hashes += 1;
            match self.hash_error.take() {
                Some(error) => Err(error),
                None => Ok(self.observed_digest.expect("test digest is configured")),
            }
        }

        fn containment_preflight(&mut self, _: &AdmittedChildLaunchV1) -> Result<(), String> {
            self.preflights += 1;
            match self.preflight_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    fn assert_rejected_without_effects(
        request: ChildLaunchRequestV1,
        decision: ChildLaunchAuthorityDecisionV1<'_>,
        expected: ChildLaunchAdmissionErrorV1,
    ) {
        let mut probe = EffectProbe::successful();
        assert_eq!(
            prepare_child_launch_request_v1(request, decision, &mut probe),
            Err(expected)
        );
        probe.assert_zero_effects();
    }

    #[test]
    fn disabled_request_is_inert_even_when_every_other_field_is_invalid() {
        let request = ChildLaunchRequestV1::new(
            99,
            ChildLaunchModeV1::Disabled,
            "stdio-peer",
            "not-a-digest",
            vec!["bad\0argument".to_string()],
            "relative-cwd",
        );
        let profile = profile();
        assert_rejected_without_effects(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            ChildLaunchAdmissionErrorV1::Disabled,
        );
    }

    #[test]
    fn authority_denial_precedes_version_or_syntax_inspection() {
        let request = ChildLaunchRequestV1::new(
            99,
            ChildLaunchModeV1::Enabled,
            "./stdio-peer",
            "bad",
            Vec::new(),
            "relative-cwd",
        );
        assert_rejected_without_effects(
            request,
            ChildLaunchAuthorityDecisionV1::Deny,
            ChildLaunchAdmissionErrorV1::AuthorityDenied,
        );
    }

    #[test]
    fn unsupported_version_is_rejected_before_preparation() {
        let mut request = valid_request();
        request.version = 2;
        let profile = profile();
        assert_rejected_without_effects(
            request,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            ChildLaunchAdmissionErrorV1::UnsupportedVersion { observed: 2 },
        );
    }

    #[test]
    fn path_style_and_non_normalized_executables_are_inert() {
        for executable in [
            "stdio-peer",
            "./stdio-peer",
            "../bin/stdio-peer",
            "/opt/kitsu-fixtures/../stdio-peer",
            "",
        ] {
            let mut request = valid_request();
            request.executable = PathBuf::from(executable);
            let profile = profile();
            assert_rejected_without_effects(
                request,
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::InvalidExecutablePath,
            );
        }
    }

    #[test]
    fn invalid_digest_forms_are_rejected_before_preparation() {
        for digest in [
            "",
            "ab",
            "ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB",
            "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
        ] {
            let mut request = valid_request();
            request.executable_sha256 = digest.to_string();
            let profile = profile();
            assert_rejected_without_effects(
                request,
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::InvalidExecutableSha256,
            );
        }
    }

    #[test]
    fn malformed_arguments_and_working_directory_are_inert() {
        let profile = profile();
        let mut nul_argument = valid_request();
        nul_argument.arguments = vec!["bad\0argument".to_string()];
        assert_rejected_without_effects(
            nul_argument,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            ChildLaunchAdmissionErrorV1::InvalidArguments,
        );

        let mut relative_cwd = valid_request();
        relative_cwd.working_directory = PathBuf::from("relative");
        assert_rejected_without_effects(
            relative_cwd,
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            ChildLaunchAdmissionErrorV1::InvalidWorkingDirectory,
        );
    }

    #[test]
    fn every_authority_mismatch_is_inert() {
        let cases = [
            (
                AuthorityProfileV1::exact(
                    "/opt/kitsu-fixtures/other",
                    DIGEST,
                    vec!["--stdio".to_string()],
                    WORKING_DIRECTORY,
                )
                .unwrap(),
                AuthorityMismatchFieldV1::Executable,
            ),
            (
                AuthorityProfileV1::exact(
                    EXECUTABLE,
                    "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                    vec!["--stdio".to_string()],
                    WORKING_DIRECTORY,
                )
                .unwrap(),
                AuthorityMismatchFieldV1::ExecutableSha256,
            ),
            (
                AuthorityProfileV1::exact(
                    EXECUTABLE,
                    DIGEST,
                    vec!["--different".to_string()],
                    WORKING_DIRECTORY,
                )
                .unwrap(),
                AuthorityMismatchFieldV1::Arguments,
            ),
            (
                AuthorityProfileV1::exact(
                    EXECUTABLE,
                    DIGEST,
                    vec!["--stdio".to_string()],
                    "/tmp/other",
                )
                .unwrap(),
                AuthorityMismatchFieldV1::WorkingDirectory,
            ),
        ];
        for (profile, field) in cases {
            assert_rejected_without_effects(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                ChildLaunchAdmissionErrorV1::AuthorityMismatch(field),
            );
        }
    }

    #[test]
    fn successful_preparation_observes_exact_identity_without_spawning() {
        let profile = profile();
        let mut probe = EffectProbe::successful();
        let prepared = prepare_child_launch_request_v1(
            valid_request(),
            ChildLaunchAuthorityDecisionV1::Admit(&profile),
            &mut probe,
        )
        .unwrap();

        assert_eq!(prepared.canonical_executable, Path::new(EXECUTABLE));
        assert_eq!(prepared.executable_sha256.0, digest_bytes());
        assert_eq!(prepared.arguments, ["--stdio"]);
        assert_eq!(prepared.working_directory, PathBuf::from(WORKING_DIRECTORY));
        assert_eq!(probe.canonicalizations, 1);
        assert_eq!(probe.hashes, 1);
        assert_eq!(probe.preflights, 1);
        assert_eq!(probe.spawns, 0);
    }

    #[test]
    fn changed_canonical_path_stops_before_hash_and_preflight() {
        let profile = profile();
        let mut probe = EffectProbe {
            canonical_path: Some(PathBuf::from("/different/stdio-peer")),
            observed_digest: Some(digest_bytes()),
            ..EffectProbe::default()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut probe,
            ),
            Err(ChildLaunchAdmissionErrorV1::ExecutablePathChanged)
        );
        assert_eq!(probe.canonicalizations, 1);
        assert_eq!(probe.hashes, 0);
        assert_eq!(probe.preflights, 0);
        assert_eq!(probe.spawns, 0);
    }

    #[test]
    fn digest_mismatch_stops_before_containment_preflight() {
        let profile = profile();
        let mut probe = EffectProbe {
            canonical_path: Some(PathBuf::from(EXECUTABLE)),
            observed_digest: Some([0xcd; 32]),
            ..EffectProbe::default()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut probe,
            ),
            Err(ChildLaunchAdmissionErrorV1::ExecutableDigestMismatch)
        );
        assert_eq!(probe.canonicalizations, 1);
        assert_eq!(probe.hashes, 1);
        assert_eq!(probe.preflights, 0);
        assert_eq!(probe.spawns, 0);
    }

    #[test]
    fn preparation_failures_do_not_advance_later_effects() {
        let profile = profile();
        let mut canonicalization_failure = EffectProbe {
            canonicalization_error: Some("unavailable".to_string()),
            ..EffectProbe::successful()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut canonicalization_failure,
            ),
            Err(ChildLaunchAdmissionErrorV1::ExecutableCanonicalization(
                "unavailable".to_string()
            ))
        );
        assert_eq!(canonicalization_failure.canonicalizations, 1);
        assert_eq!(canonicalization_failure.hashes, 0);
        assert_eq!(canonicalization_failure.preflights, 0);

        let mut hash_failure = EffectProbe {
            hash_error: Some("unreadable".to_string()),
            ..EffectProbe::successful()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut hash_failure,
            ),
            Err(ChildLaunchAdmissionErrorV1::ExecutableHash(
                "unreadable".to_string()
            ))
        );
        assert_eq!(hash_failure.canonicalizations, 1);
        assert_eq!(hash_failure.hashes, 1);
        assert_eq!(hash_failure.preflights, 0);

        let mut preflight_failure = EffectProbe {
            preflight_error: Some("containment unavailable".to_string()),
            ..EffectProbe::successful()
        };
        assert_eq!(
            prepare_child_launch_request_v1(
                valid_request(),
                ChildLaunchAuthorityDecisionV1::Admit(&profile),
                &mut preflight_failure,
            ),
            Err(ChildLaunchAdmissionErrorV1::ContainmentPreflight(
                "containment unavailable".to_string()
            ))
        );
        assert_eq!(preflight_failure.canonicalizations, 1);
        assert_eq!(preflight_failure.hashes, 1);
        assert_eq!(preflight_failure.preflights, 1);
        assert_eq!(preflight_failure.spawns, 0);
    }
}
