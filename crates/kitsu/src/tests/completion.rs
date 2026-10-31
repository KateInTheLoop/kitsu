use super::*;

#[test]
fn required_write_sha256_parser_splits_final_equals_and_normalizes_hex() {
    let (path, sha256) =
        parse_required_write_sha256(&format!("/tmp/output=part={}", "AB".repeat(32)))
            .unwrap_or_else(|error| panic!("valid exact write expectation: {error}"));
    assert_eq!(path, "/tmp/output=part");
    assert_eq!(sha256, "ab".repeat(32));

    for invalid in [
        "missing-separator",
        "=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "/tmp/output=abc",
        "/tmp/output=gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
    ] {
        assert!(
            parse_required_write_sha256(invalid).is_err(),
            "must reject {invalid:?}"
        );
    }
}
