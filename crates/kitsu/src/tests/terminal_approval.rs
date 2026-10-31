use super::*;

#[test]
fn permission_answers_require_exact_tokens() {
    assert_eq!(parse_permission_answer("y\n"), Some(Decision::AllowOnce));
    assert_eq!(parse_permission_answer("YES"), Some(Decision::AllowOnce));
    assert_eq!(
        parse_permission_answer("always"),
        Some(Decision::AllowAlways)
    );
    assert_eq!(parse_permission_answer("a"), Some(Decision::AllowAlways));
    assert_eq!(parse_permission_answer("no"), Some(Decision::Deny));
    assert_eq!(parse_permission_answer("another pasted line"), None);
    assert_eq!(parse_permission_answer("yes please"), None);
}
