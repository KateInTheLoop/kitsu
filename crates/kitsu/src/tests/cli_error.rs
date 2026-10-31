use super::*;

#[test]
fn cli_errors_preserve_neutral_outcome_class_independently_from_exit_code() {
    assert_eq!(
        CliError::user("bad input").outcome_class(),
        kitsu_core::RunOutcomeClass::UserFault
    );
    assert_eq!(
        CliError::provider("gateway failed").outcome_class(),
        kitsu_core::RunOutcomeClass::ProviderFault
    );
    assert_eq!(
        CliError::tool("tool failed").outcome_class(),
        kitsu_core::RunOutcomeClass::ToolFault
    );
    assert_eq!(
        CliError::cancelled("cancelled").outcome_class(),
        kitsu_core::RunOutcomeClass::Cancelled
    );
    assert_eq!(
        CliError::interrupted("input closed").outcome_class(),
        kitsu_core::RunOutcomeClass::Interrupted
    );
    assert_eq!(CliError::user("bad input").exit_code(), EXIT_USER);
    assert_eq!(CliError::tool("tool failed").exit_code(), EXIT_USER);
}

#[test]
fn command_env_spec_parses_bare_and_explicit_forms() {
    assert!(matches!(
        expect_ok(CommandEnvSpec::parse("PYTHONPATH")),
        CommandEnvSpec::Passthrough(name) if name == "PYTHONPATH"
    ));
    assert!(matches!(
        expect_ok(CommandEnvSpec::parse("PYTHONPATH=/venv/site-packages")),
        CommandEnvSpec::Explicit(name, value)
            if name == "PYTHONPATH" && value == "/venv/site-packages"
    ));
    // A value may itself contain '=' (e.g. a joined PATH-like value).
    assert!(matches!(
        expect_ok(CommandEnvSpec::parse("FOO=a=b")),
        CommandEnvSpec::Explicit(name, value) if name == "FOO" && value == "a=b"
    ));
    assert!(CommandEnvSpec::parse("").is_err());
    assert!(CommandEnvSpec::parse("=value").is_err());
}
