use super::*;

#[test]
fn assistive_line_reader_is_literal_escape_free_stdio() {
    let mut input = std::io::Cursor::new(b"hello world\r\n".to_vec());
    let mut output = Vec::new();
    let line = read_assistive_line_from(
        &mut input,
        &mut output,
        "\u{1b}[31m[UNSAVED]\u{1b}[0m kitsu> ",
    )
    .unwrap();
    assert_eq!(line.as_deref(), Some("hello world"));
    assert!(!output.contains(&0x1b));
    assert_eq!(String::from_utf8(output).unwrap(), "[UNSAVED] kitsu> ");

    let mut eof = std::io::Cursor::new(Vec::<u8>::new());
    let mut prompt = Vec::new();
    assert_eq!(
        read_assistive_line_from(&mut eof, &mut prompt, "kitsu> ").unwrap(),
        None
    );
    assert_eq!(prompt, b"kitsu> ");
}
