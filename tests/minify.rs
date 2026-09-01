//! The standalone minifier: taking the whitespace back out of JSON text.
//!
//! The property that matters most is that minifying is the exact inverse of
//! laying out. Whatever policy a document was written under, and however it was
//! spaced before that, minifying it has to land on the compact form the writer
//! itself would have produced. That is checked against the writer directly,
//! over every shape a document can take.
//!
//! The rest is what a minifier sees that a writer never does: whitespace that
//! belongs to a string rather than to the layout, comments, input that is not
//! JSON at all, and the one case where dropping whitespace would rewrite the
//! document instead of reformatting it.

use std::collections::BTreeMap;

use structio::json::{minify_into, minify_into_with, minify_with};
use structio::{
    AllowComments, ErrorCode, Options, Pretty, PrettyInlineArrays, Standard, from_str, minify,
    prettify_with, to_string, to_string_with,
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
    labels: BTreeMap<String, u8>,
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

fn sample() -> Outer {
    Outer {
        id: 7,
        rows: vec![
            Inner {
                // Spaces, a quote and a tab inside a string: the whitespace a
                // minifier must not touch, next to the bytes it must not
                // mistake for structure.
                name: "a \"b\" c\t".into(),
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
        labels: [("j k".to_string(), 2), ("l".to_string(), 1)]
            .into_iter()
            .collect(),
    }
}

/// Minifying text laid out under `O` gets back to the writer's compact form,
/// and doing it again changes nothing.
fn inverts_the_writer<O: Options>(label: &str) {
    let value = sample();
    let compact = to_string(&value);
    let laid_out = to_string_with::<O, _>(&value);

    assert_eq!(
        minify(&laid_out).unwrap(),
        compact,
        "{label}: minified text differs from the writer's compact output"
    );
    // The two ways to compact a document have to agree, or the crate has two
    // answers to the same question.
    assert_eq!(
        minify(&laid_out).unwrap(),
        prettify_with::<Standard>(&laid_out).unwrap(),
        "{label}: the minifier and the compact writer disagree"
    );
}

#[test]
fn minifying_is_the_writers_compact_form() {
    inverts_the_writer::<Pretty>("Pretty");
    inverts_the_writer::<PrettyInlineArrays>("PrettyInlineArrays");
    inverts_the_writer::<Wide>("Wide");
    inverts_the_writer::<WideInline>("WideInline");
    // Minifying text that is already minified moves nothing. `Standard` as the
    // layout says exactly that, so it is said once rather than once per policy.
    assert_eq!(minify(&to_string(&sample())).unwrap(), to_string(&sample()));
}

// ---------------------------------------------------------------------------
// Whitespace
// ---------------------------------------------------------------------------

#[test]
fn whitespace_between_tokens_is_dropped_wherever_it_is() {
    // Every position a formatter could have put whitespace in, at once.
    let spaced = " \t\r\n{ \"a\" : [ 1 , 2 ] , \"b\" : { } , \"c\" : [ ] } \n";
    assert_eq!(minify(spaced).unwrap(), r#"{"a":[1,2],"b":{},"c":[]}"#);
}

#[test]
fn whitespace_inside_a_string_is_the_documents_own() {
    assert_eq!(
        minify("[ \" a\\tb \\n c \" ]").unwrap(),
        "[\" a\\tb \\n c \"]"
    );
    // A literal tab or newline is not legal JSON inside a string, but it is not
    // the minifier's job to say so. What matters is that it survives: this is
    // the whitespace that would change the document if it were dropped.
    for doc in ["[\"a\tb\"]", "[\"a\nb\"]"] {
        assert_eq!(minify(doc).unwrap(), doc, "{doc:?}");
        assert!(from_str::<Vec<String>>(doc).is_err(), "{doc:?}");
    }
}

#[test]
fn structural_bytes_inside_a_string_are_not_structure() {
    let doc = r#"{ "{[,:]}" : " } ] " }"#;
    assert_eq!(minify(doc).unwrap(), r#"{"{[,:]}":" } ] "}"#);
}

#[test]
fn an_escaped_quote_does_not_end_the_string() {
    assert_eq!(minify(r#"[ "a\"  b" ]"#).unwrap(), r#"["a\"  b"]"#);
    assert_eq!(minify(r#"[ "a\\" , 1 ]"#).unwrap(), r#"["a\\",1]"#);
}

#[test]
fn a_string_long_enough_to_cross_the_word_boundary_is_still_one_token() {
    // The copy runs eight bytes at a time; a string that spans several of them
    // with its spaces intact is what catches a scan that resynchronized.
    let body = "the quick brown fox jumps over the lazy dog, twice over";
    let doc = format!("[ \"{body}\" , \"{body}\" ]");
    assert_eq!(minify(&doc).unwrap(), format!("[\"{body}\",\"{body}\"]"));
}

// ---------------------------------------------------------------------------
// What is copied through
// ---------------------------------------------------------------------------

#[test]
fn a_token_longer_than_the_largest_copy_block_still_comes_through() {
    // Runs are copied in fixed-size blocks up to 64 bytes; past that the copy
    // falls back to a plain one, which nothing else here reaches on purpose.
    let long = "x".repeat(300);
    let doc = format!("[ \"{long}\" , 1 ]");
    assert_eq!(minify(&doc).unwrap(), format!("[\"{long}\",1]"));

    // And a bare token that long, which is a run rather than a string.
    let digits = "1".repeat(300);
    assert_eq!(
        minify(&format!("[ {digits} ]")).unwrap(),
        format!("[{digits}]")
    );
}

#[test]
fn a_number_keeps_the_spelling_the_input_gave_it() {
    let doc = "[ 1.50 , 1e3 , -0.0 , 1E+2 , 00.10 ]";
    assert_eq!(minify(doc).unwrap(), "[1.50,1e3,-0.0,1E+2,00.10]");
}

#[test]
fn non_ascii_text_survives_the_copy() {
    let doc = "{ \"kéy\" : \"välue \u{1F600}\" }";
    assert_eq!(minify(doc).unwrap(), "{\"kéy\":\"välue \u{1F600}\"}");
}

#[test]
fn a_top_level_scalar_is_a_document_too() {
    assert_eq!(minify(" 42 ").unwrap(), "42");
    assert_eq!(minify(" \"a b\" ").unwrap(), "\"a b\"");
    assert_eq!(minify(" true ").unwrap(), "true");
}

#[test]
fn empty_input_has_no_whitespace_to_remove() {
    assert_eq!(minify("").unwrap(), "");
    assert_eq!(minify("   \n\t ").unwrap(), "");
}

// ---------------------------------------------------------------------------
// Strictness, and the lack of it
// ---------------------------------------------------------------------------

#[test]
fn broken_structure_is_shortened_rather_than_refused() {
    // Minifying needs the strings located and nothing else, so none of this is
    // its business. `from_str` is the thing that has an opinion.
    for (doc, want) in [
        ("{ \"a\" : 1 , , , }", "{\"a\":1,,,}"),
        ("[ 1 , 2", "[1,2"),
        ("} ] , :", "}],:"),
        ("{ \"a\" \"b\" }", "{\"a\"\"b\"}"),
        ("[ 01 ]", "[01]"),
        ("[ tru ]", "[tru]"),
    ] {
        assert_eq!(minify(doc).unwrap(), want, "{doc:?}");
    }
}

#[test]
fn whitespace_holding_two_tokens_apart_is_not_the_layouts_to_remove() {
    // Dropping it would turn each of these into a different document, and in
    // the first case a well-formed one, from input that was not.
    for (doc, at) in [
        ("[1 2]", 3),
        ("[true false]", 6),
        ("[1\n2]", 3),
        ("1 2", 2),
        ("[1 , 2 3]", 7),
    ] {
        let e = minify(doc).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnexpectedCharacter, "{doc:?}");
        assert_eq!(e.index, at, "{doc:?}");
    }

    // Whitespace beside punctuation or a quote is holding nothing apart, since
    // those tokens end themselves.
    for doc in ["[1 , 2]", "[ 1,2 ]", "{\"a\" : 1}", "[\"a\" \"b\"]"] {
        assert!(minify(doc).is_ok(), "{doc:?}");
    }
}

#[test]
fn an_unterminated_string_is_an_error_against_the_string() {
    let e = minify(r#"{"a": "unclosed"#).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnexpectedEnd);
    assert_eq!(e.index, 7);
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

#[test]
fn comments_are_read_only_when_asked_for_and_never_written_back() {
    let doc = "{ // which\n  \"a\" : /* one */ 1 }";
    assert_eq!(minify_with::<AllowComments>(doc).unwrap(), r#"{"a":1}"#);

    // Without the setting a comment is not whitespace, so its bytes are
    // ordinary bytes and go through like any other.
    assert_eq!(
        minify_with::<Standard>("[ 1 , /*x*/ 2 ]").unwrap(),
        "[1,/*x*/2]"
    );
}

#[test]
fn a_comment_between_two_bare_tokens_is_holding_them_apart_too() {
    let e = minify_with::<AllowComments>("[1 /* two */ 2]").unwrap_err();
    assert_eq!(e.code, ErrorCode::UnexpectedCharacter);
    assert_eq!(e.index, 13);
}

#[test]
fn a_slash_that_begins_no_comment_is_refused_rather_than_guessed_at() {
    // Dropping what follows assumes a comment; keeping it assumes content.
    for doc in ["[ 1 /* never closed", "[1, /x]", "[1] /"] {
        let e = minify_with::<AllowComments>(doc).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnexpectedCharacter, "{doc:?}");
        assert_eq!(e.index, doc.find('/').unwrap(), "{doc:?}");
    }

    // Without the setting a slash starts nothing, so it is an ordinary byte.
    assert_eq!(minify_with::<Standard>("[ 1 /*x").unwrap(), "[1/*x");
}

#[test]
fn a_slash_inside_a_string_is_not_a_comment() {
    let doc = r#"{ "url" : "https://example.com/a//b" }"#;
    for out in [
        minify_with::<AllowComments>(doc).unwrap(),
        minify_with::<Standard>(doc).unwrap(),
    ] {
        assert_eq!(out, r#"{"url":"https://example.com/a//b"}"#);
    }
}

// ---------------------------------------------------------------------------
// The buffer-reusing entry points
// ---------------------------------------------------------------------------

#[test]
fn minify_into_replaces_the_contents_and_keeps_the_allocation() {
    let mut out = String::with_capacity(4096);
    let before = out.capacity();
    out.push_str("stale");

    minify_into("{ \"a\" : 1 }", &mut out).unwrap();
    assert_eq!(out, r#"{"a":1}"#);

    minify_into_with::<AllowComments>("[ 1 /* c */ , 2 ]", &mut out).unwrap();
    assert_eq!(out, "[1,2]");
    assert_eq!(out.capacity(), before, "the allocation was not reused");
}

#[test]
fn a_failed_minify_into_still_hands_the_string_back() {
    let mut out = String::new();
    let e = minify_into("[1, \"unclosed", &mut out).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnexpectedEnd);
    // Whatever was copied before the error, and a `String` that is still usable.
    assert_eq!(out, "[1,");

    minify_into("[ 2 ]", &mut out).unwrap();
    assert_eq!(out, "[2]");
}

// ---------------------------------------------------------------------------
// Size
// ---------------------------------------------------------------------------

#[test]
fn a_large_document_minifies_to_what_it_was_written_as() {
    let value = Outer {
        rows: (0..2000)
            .map(|i| Inner {
                name: format!("row {i} of many"),
                vals: vec![i as f64 / 8.0, -1.0],
                flags: vec![i % 2 == 0],
            })
            .collect(),
        ..sample()
    };
    let compact = to_string(&value);
    let laid_out = to_string_with::<Pretty, _>(&value);
    assert!(laid_out.len() > compact.len());

    assert_eq!(minify(&laid_out).unwrap(), compact);
}
