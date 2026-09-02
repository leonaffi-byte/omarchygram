use omarchygram::ui::auth::{parse_api_id, valid_api_hash_text};
use omarchygram::ui::keys::{find_conflicts, markers, wrap_markers};

#[test]
fn credential_validation_requires_positive_digits_and_a_32_char_hex_hash() {
    assert_eq!(parse_api_id("12345"), Some(12345));
    assert_eq!(parse_api_id(" 42 "), Some(42));
    assert_eq!(parse_api_id("0"), None);
    assert_eq!(parse_api_id("12x"), None);
    assert_eq!(parse_api_id("2147483648"), None);

    assert!(valid_api_hash_text("0123456789abcdef0123456789abcdef"));
    assert!(valid_api_hash_text("0123456789ABCDEF0123456789ABCDEF"));
    assert!(!valid_api_hash_text("0123456789abcdef0123456789abcde"));
    assert!(!valid_api_hash_text("0123456789abcdef0123456789abcdeg"));
}

#[test]
fn conflicts_use_canonical_accelerators_and_stay_within_a_group() {
    // These are the already-canonicalized (lowercase keyval, modifier mask)
    // values produced before conflict detection. `gtk::accelerator_parse`
    // panics before GTK initialization, so the GUI probe separately asserts
    // that <Primary>f and <Control>f canonicalize identically.
    let control_f = Some((u32::from(b'f'), 1 << 2));
    let rows = vec![
        ("search".into(), "Global".into(), control_f),
        ("switcher".into(), "Global".into(), control_f),
        ("composer".into(), "Composer".into(), control_f),
        ("unbound".into(), "Global".into(), None),
    ];
    assert_eq!(find_conflicts(&rows), vec![(0, 1)]);
}

#[test]
fn marker_insertion_wraps_a_selection_and_places_an_empty_cursor_inside() {
    let (bold, cursor) = wrap_markers("say hello now", 4, 9, "**", "**");
    assert_eq!(bold, "say **hello** now");
    assert_eq!(cursor, 13);

    let (unicode, cursor) = wrap_markers("aéz", 1, 3, "__", "__");
    assert_eq!(unicode, "a__é__z");
    assert_eq!(cursor, 7);

    let (empty, cursor) = wrap_markers("text", 2, 2, "`", "`");
    assert_eq!(empty, "te``xt");
    assert_eq!(cursor, 3);

    let (reversed, cursor) = wrap_markers("abcd", 3, 1, "||", "||");
    assert_eq!(reversed, "a||bc||d");
    assert_eq!(cursor, 7);

    assert_eq!(markers("link"), Some(("[", "](url)")));
    assert_eq!(markers("underline"), None);
}
