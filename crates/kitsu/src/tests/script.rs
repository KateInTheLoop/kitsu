use super::*;

#[test]
fn failure_count_phrase_pluralizes_naturally() {
    assert_eq!(failure_count_phrase(1), "1 failure");
    assert_eq!(failure_count_phrase(2), "2 failures");
}
