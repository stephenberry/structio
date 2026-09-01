//! The standalone prettifier: laying out JSON text that arrived as text.
//!
//! The property that matters most is not any one document's spacing. It is
//! that prettifying text produces exactly what *writing the same data* under
//! the same policy would have produced, because that is what makes the layout
//! one set of rules instead of two. Most of what follows checks that, over
//! every shape a document can take, against the writer itself.
//!
//! The rest is the half a writer never has to think about: what happens to
//! input it did not produce. Whitespace already in the document, escapes and
//! structural bytes inside strings, numbers spelled in ways no formatter would
//! spell them, and text that is not JSON at all.

use std::collections::BTreeMap;

use structio::json::{prettify_into, prettify_into_with, prettify_with};
use structio::{
    AllowComments, ErrorCode, Options, Pretty, PrettyInlineArrays, Standard, from_str, prettify,
    to_string, to_string_with,
};

/// Four spaces, to catch anything that assumed the default width.
#[derive(Clone, Copy)]
struct Wide;

impl Options for Wide {
    const PRETTY: bool = true;
    const INDENT: usize = 4;
}

/// Four spaces with arrays inline, since the two settings have to compose.
#[derive(Clone, Copy)]
struct WideInline;

impl Options for WideInline {
    const PRETTY: bool = true;
    const INDENT: usize = 4;
    const NEW_LINES_IN_ARRAYS: bool = false;
}

// ---------------------------------------------------------------------------
// The document every policy is checked against
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Inner {
    name: String,
    vals: Vec<f64>,
    flags: Vec<bool>,
}
structio::object!(Inner { name, vals, flags });

#[derive(Default)]
struct Outer {
    id: u32,
    rows: Vec<Inner>,
    empty_array: Vec<u8>,
    empty_object: BTreeMap<String, u8>,
    nothing: Option<u8>,
    nested: Vec<Vec<i32>>,
    labels: BTreeMap<String, i32>,
}
structio::object!(Outer {
    id,
    rows,
    empty_array,
    empty_object,
    nothing,
    nested,
    labels
});

/// One value holding every shape that has its own path through the writer: an
/// object of objects, arrays of scalars, arrays of arrays, a map, a null, a
/// string that needs escaping, and both containers empty.
fn sample() -> Outer {
    Outer {
        id: 7,
        rows: vec![
            Inner {
                name: "a\"b\n\t".into(),
                vals: vec![1.5, -2.0, 0.0],
                flags: vec![true, false],
            },
            Inner {
                name: String::new(),
                vals: vec![],
                flags: vec![true],
            },
        ],
        empty_array: vec![],
        empty_object: BTreeMap::new(),
        nothing: None,
        nested: vec![vec![1, 2], vec![], vec![3]],
        labels: [("j".to_string(), 2), ("k".to_string(), 1)]
            .into_iter()
            .collect(),
    }
}

/// Prettifying the compact form gives what writing under `O` gives, and doing
/// it again to the result changes nothing.
fn agrees_with_the_writer<O: Options>(label: &str) {
    let value = sample();
    let compact = to_string(&value);
    let want = to_string_with::<O, _>(&value);

    assert_eq!(
        prettify_with::<O>(&compact).unwrap(),
        want,
        "{label}: prettified text differs from written output"
    );
    assert_eq!(
        prettify_with::<O>(&want).unwrap(),
        want,
        "{label}: prettifying laid-out text moved it"
    );
    assert_eq!(
        prettify_with::<Standard>(&want).unwrap(),
        compact,
        "{label}: compacting the laid-out text did not get back to the original"
    );
}

#[test]
fn the_layout_is_the_writers_layout() {
    agrees_with_the_writer::<Pretty>("Pretty");
    agrees_with_the_writer::<PrettyInlineArrays>("PrettyInlineArrays");
    agrees_with_the_writer::<Wide>("Wide");
    agrees_with_the_writer::<WideInline>("WideInline");
    agrees_with_the_writer::<Standard>("Standard");
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

#[test]
fn a_document_is_laid_out_across_lines() {
    assert_eq!(
        prettify(r#"{"a":[1,2],"b":{"c":null}}"#).unwrap(),
        "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {\n    \"c\": null\n  }\n}"
    );
}

#[test]
fn an_empty_container_stays_on_one_line() {
    assert_eq!(prettify("{}").unwrap(), "{}");
    assert_eq!(prettify("[]").unwrap(), "[]");
    assert_eq!(
        prettify(r#"{"a":{},"b":[]}"#).unwrap(),
        "{\n  \"a\": {},\n  \"b\": []\n}"
    );
}

#[test]
fn a_top_level_scalar_is_a_document_too() {
    for doc in ["1", "-2.5e10", "true", "false", "null", r#""hi""#] {
        assert_eq!(prettify(doc).unwrap(), doc, "{doc}");
        assert_eq!(prettify_with::<Standard>(doc).unwrap(), doc, "{doc}");
    }
}

#[test]
fn the_indent_width_comes_from_the_policy() {
    assert_eq!(
        prettify_with::<Wide>(r#"{"a":[1]}"#).unwrap(),
        "{\n    \"a\": [\n        1\n    ]\n}"
    );
}

#[test]
fn an_inline_array_keeps_its_elements_on_one_line() {
    assert_eq!(
        prettify_with::<PrettyInlineArrays>(r#"{"v":[1,2,3],"w":[]}"#).unwrap(),
        "{\n  \"v\": [1, 2, 3],\n  \"w\": []\n}"
    );
    // An object inside one still breaks, and takes its level from the line the
    // array began on.
    assert_eq!(
        prettify_with::<PrettyInlineArrays>(r#"[{"a":1},{"a":2}]"#).unwrap(),
        "[{\n  \"a\": 1\n}, {\n  \"a\": 2\n}]"
    );
}

#[test]
fn a_compact_policy_compacts() {
    let pretty = "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": \"x y\"\n}";
    assert_eq!(
        prettify_with::<Standard>(pretty).unwrap(),
        r#"{"a":[1,2],"b":"x y"}"#
    );
}

#[test]
fn nesting_is_indented_by_depth_however_deep() {
    let deep = format!("{}1{}", "[".repeat(8), "]".repeat(8));
    let out = prettify(&deep).unwrap();
    // Level `n` opens at 2n spaces, and the innermost value sits one further.
    // Level zero is the first byte, so it has no line of its own to check.
    for level in 1..8 {
        assert!(
            out.contains(&format!("\n{}[", " ".repeat(level * 2))),
            "level {level} missing from\n{out}"
        );
    }
    // The innermost value sits at level eight, and the bracket that closes
    // around it lines up with the line its own bracket opened on.
    assert!(
        out.contains(&format!("\n{}1\n{}]", " ".repeat(16), " ".repeat(14))),
        "{out}"
    );
    assert!(out.ends_with("\n]"), "{out}");
    assert_eq!(prettify_with::<Standard>(&out).unwrap(), deep);
}

// ---------------------------------------------------------------------------
// What the input brought with it
// ---------------------------------------------------------------------------

#[test]
fn whatever_whitespace_the_input_had_is_replaced() {
    // Tabs, carriage returns, blank lines, and spaces in every position that
    // allows one.
    let messy = "  {\r\n\t\"a\"  :\t[ 1 ,\n\n 2 ]  ,  \"b\" : { }  }  \n";
    assert_eq!(
        prettify(messy).unwrap(),
        "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}"
    );
}

#[test]
fn a_string_is_copied_byte_for_byte() {
    // Everything inside quotes is data: structural bytes, escapes that a
    // writer would have spelled differently, and characters past ASCII.
    let doc = r#"{"k{[,:}]":"A\\\"\n\/ tail","é☃":"日本"}"#;
    let out = prettify(doc).unwrap();
    assert!(out.contains(r#""k{[,:}]": "A\\\"\n\/ tail""#), "{out}");
    assert!(out.contains(r#""é☃": "日本""#), "{out}");
    assert_eq!(prettify_with::<Standard>(&out).unwrap(), doc);
}

#[test]
fn an_escaped_quote_does_not_end_the_string() {
    // A backslash run of even length leaves the quote closing, of odd length
    // leaves it escaped. Both spellings have to come through whole.
    let doc = r#"["a\\","b\\\"c",""]"#;
    assert_eq!(prettify_with::<Standard>(doc).unwrap(), doc);
}

#[test]
fn a_number_keeps_the_spelling_the_input_gave_it() {
    // None of these is how this crate's formatter would write the value, and
    // all of them are the value the document says.
    let doc = "[1.50,1e5,1E+5,-0,0.0,123456789012345678901234567890,1e-400]";
    assert_eq!(prettify_with::<Standard>(doc).unwrap(), doc);
}

#[test]
fn a_malformed_number_is_laid_out_rather_than_refused() {
    // A number is stepped over by its alphabet, not held to the grammar. The
    // walk copies whatever the token was, so a document the reader rejects
    // still lays out, and is still rejected by whoever reads it next. The
    // check would have cost every well-formed number in every document to
    // move that rejection one step earlier.
    for doc in ["[01]", "[1.]", "[1e]", "[1e+]", "[-]", "[1.2.3]"] {
        assert_eq!(prettify_with::<Standard>(doc).unwrap(), doc, "{doc}");
        assert!(from_str::<Vec<f64>>(doc).is_err(), "{doc}");
    }

    // A byte that begins no value at all is still refused. That is structure,
    // which the walk has to know before it can lay anything out at all.
    for doc in ["[.5]", "[+1]"] {
        assert_eq!(
            prettify(doc).unwrap_err().code,
            ErrorCode::UnexpectedCharacter,
            "{doc}"
        );
    }
}

// ---------------------------------------------------------------------------
// Text that is not a document
// ---------------------------------------------------------------------------

#[test]
fn malformed_input_is_an_error_against_the_byte_that_stopped_it() {
    for (doc, code, index) in [
        ("", ErrorCode::UnexpectedEnd, 0),
        ("   ", ErrorCode::UnexpectedEnd, 3),
        ("{", ErrorCode::UnexpectedEnd, 1),
        (r#"{"a":1"#, ErrorCode::UnexpectedEnd, 6),
        (r#"{a:1}"#, ErrorCode::ExpectedQuote, 1),
        (r#"{"a" 1}"#, ErrorCode::ExpectedColon, 5),
        (r#"{"a":}"#, ErrorCode::UnexpectedCharacter, 5),
        (r#"{"a":1,}"#, ErrorCode::ExpectedQuote, 7),
        ("[1,]", ErrorCode::UnexpectedCharacter, 3),
        ("[1 2]", ErrorCode::ExpectedComma, 3),
        ("[}", ErrorCode::UnexpectedCharacter, 1),
        ("}", ErrorCode::UnexpectedCharacter, 0),
        ("tru", ErrorCode::ExpectedTrue, 0),
        (r#""abc"#, ErrorCode::UnexpectedEnd, 1),
        ("\"a\tb\"", ErrorCode::ControlCharacterInString, 1),
        (r#"{"a":1}x"#, ErrorCode::TrailingContent, 7),
        // Two documents in one string is one document with a tail.
        (r#"{"a":1}{"b":2}"#, ErrorCode::TrailingContent, 7),
    ] {
        let e = prettify(doc).unwrap_err();
        assert_eq!((e.code, e.index), (code, index), "{doc:?}");
    }
}

#[test]
fn nesting_past_the_limit_is_refused_rather_than_recursed() {
    // The walk descends with the document, so it is held to the same depth
    // limit the parser is, and by the same counter.
    let deep = format!("{}{}", "[".repeat(300), "]".repeat(300));
    assert_eq!(
        prettify(&deep).unwrap_err().code,
        ErrorCode::ExceededMaxDepth
    );

    let ok = format!("{}{}", "[".repeat(200), "]".repeat(200));
    assert!(prettify(&ok).is_ok());
}

#[test]
fn comments_are_read_only_when_asked_for_and_never_written_back() {
    let doc = "{\n // which port\n \"port\": 8080 /* here */\n}";

    assert_eq!(prettify(doc).unwrap_err().code, ErrorCode::ExpectedQuote);
    assert_eq!(
        prettify_with::<AllowComments>(doc).unwrap(),
        r#"{"port":8080}"#,
        "AllowComments is not a pretty policy, so this is the compact form"
    );
}

// ---------------------------------------------------------------------------
// The buffer-reusing entry point
// ---------------------------------------------------------------------------

#[test]
fn prettify_into_replaces_the_contents_and_keeps_the_allocation() {
    let mut out = String::with_capacity(4096);
    let before = out.capacity();
    out.push_str("stale");

    prettify_into(r#"{"a":1}"#, &mut out).unwrap();
    assert_eq!(out, "{\n  \"a\": 1\n}");
    assert_eq!(out.capacity(), before, "the buffer was reallocated");

    prettify_into_with::<Standard>(r#"{"b":[1,2]}"#, &mut out).unwrap();
    assert_eq!(out, r#"{"b":[1,2]}"#);
}

#[test]
fn a_failed_prettify_into_still_hands_the_string_back() {
    let mut out = String::from("stale");
    let e = prettify_into(r#"{"a":1,,}"#, &mut out).unwrap_err();
    assert_eq!(e.code, ErrorCode::ExpectedQuote);
    // Whatever is there is what was laid out before the error, and it is at
    // least a `String` that can be cleared and used again.
    assert!(!out.contains("stale"));
    out.clear();
    prettify_into("[]", &mut out).unwrap();
    assert_eq!(out, "[]");
}

// ---------------------------------------------------------------------------
// Scale
// ---------------------------------------------------------------------------

#[test]
fn a_large_document_lays_out_the_same_as_it_writes() {
    // Enough of everything that the writer's buffer grows several times, so
    // the output cannot depend on where a reallocation happened to land.
    #[derive(Default)]
    struct Row {
        i: u64,
        text: String,
        xs: Vec<f64>,
    }
    structio::object!(Row { i, text, xs });

    let rows: Vec<Row> = (0..2_000u64)
        .map(|i| Row {
            i,
            text: format!("row \"{i}\"\n"),
            xs: (0..i % 17).map(|k| k as f64 / 8.0).collect(),
        })
        .collect();

    let compact = to_string(&rows);
    assert!(compact.len() > 64 * 1024);

    for (label, want, got) in [
        (
            "Pretty",
            to_string_with::<Pretty, _>(&rows),
            prettify(&compact).unwrap(),
        ),
        (
            "PrettyInlineArrays",
            to_string_with::<PrettyInlineArrays, _>(&rows),
            prettify_with::<PrettyInlineArrays>(&compact).unwrap(),
        ),
    ] {
        assert_eq!(got, want, "{label}");
        assert_eq!(prettify_with::<Standard>(&got).unwrap(), compact, "{label}");
    }
}
