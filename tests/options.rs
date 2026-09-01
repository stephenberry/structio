//! Policies: indentation, members that drop out, and keys nothing claims.
//!
//! Every setting is an associated constant on a type, so the interesting cases
//! are the ones where a policy changes the *shape* of a document rather than
//! its spacing: an object whose member count has to account for what was left
//! out, and a container that is empty because everything in it was.
//!
//! The read half has three settings, and their interest is the opposite: where
//! an unknown key is *not* one, where an absent member is *not* missing
//! because the thing holding it is not an object with a schema, and where a
//! `/` is *not* a comment.

use std::collections::BTreeMap;

use structio::{
    AllowComments, Documents, ErrorCode, Options, Pretty, PrettyInlineArrays, RequireKeys,
    SkipNull, SkipUnknown, Standard, beve, from_beve, from_beve_with, from_str, from_str_with,
    to_beve, to_beve_with, to_string, to_string_with, transcode::beve_to_json_with,
};

/// Indented four spaces with nulls left out, which is the combination the
/// built-in policies deliberately do not provide: a user writes it.
#[derive(Clone, Copy)]
struct Config;

impl Options for Config {
    const PRETTY: bool = true;
    const INDENT: usize = 4;
    const SKIP_NULL: bool = true;
}

/// `PrettyInlineArrays` at a different width, since the two settings have to
/// compose: an inline array is still inside a document indented four spaces.
#[derive(Clone, Copy)]
struct WideInline;

impl Options for WideInline {
    const PRETTY: bool = true;
    const INDENT: usize = 4;
    const NEW_LINES_IN_ARRAYS: bool = false;
}

/// Arrays inline and nothing else indented, which is ordinary compact JSON:
/// there are no lines for an array to be kept off.
#[derive(Clone, Copy)]
struct CompactInline;

impl Options for CompactInline {
    const NEW_LINES_IN_ARRAYS: bool = false;
}

#[derive(Default, Debug, PartialEq)]
struct Sensor {
    name: String,
    reading: f64,
    note: Option<String>,
}
structio::object!(Sensor {
    name,
    reading,
    note
});

#[derive(Default, Debug, PartialEq)]
struct Nest {
    inner: Sensor,
    tags: Vec<String>,
}
structio::object!(Nest { inner, tags });

fn sensor() -> Sensor {
    Sensor {
        name: "t1".into(),
        reading: 21.5,
        note: None,
    }
}

// ---------------------------------------------------------------------------
// PRETTY
// ---------------------------------------------------------------------------

#[test]
fn indentation_follows_nesting_depth() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into()],
    };
    assert_eq!(
        to_string_with::<Pretty, _>(&value),
        "{\n  \"inner\": {\n    \"name\": \"t1\",\n    \"reading\": 21.5,\n    \
         \"note\": null\n  },\n  \"tags\": [\n    \"a\"\n  ]\n}"
    );
}

#[test]
fn the_indent_width_is_the_policys_to_choose() {
    // Same document, same shape, four spaces per level instead of two, and the
    // null member gone because this policy also asks for that.
    assert_eq!(
        to_string_with::<Config, _>(&sensor()),
        "{\n    \"name\": \"t1\",\n    \"reading\": 21.5\n}"
    );
}

/// A container with nothing in it has nothing to indent, and a newline before
/// its closing bracket would be indenting nothing.
#[test]
fn an_empty_container_stays_on_one_line() {
    #[derive(Default)]
    struct Empty {
        items: Vec<u8>,
        map: BTreeMap<String, u8>,
    }
    structio::object!(Empty { items, map });

    assert_eq!(
        to_string_with::<Pretty, _>(&Empty::default()),
        "{\n  \"items\": [],\n  \"map\": {}\n}"
    );
}

/// Every member of this one is skipped, so the object is empty by the same
/// route an empty `Vec` is, and must close the same way.
#[test]
fn an_object_emptied_by_skipping_is_still_an_empty_object() {
    #[derive(Default)]
    struct AllOptional {
        a: Option<u8>,
        b: Option<u8>,
    }
    structio::object!(AllOptional { a, b });

    assert_eq!(to_string_with::<SkipNull, _>(&AllOptional::default()), "{}");
    assert_eq!(to_string_with::<Config, _>(&AllOptional::default()), "{}");
    assert_eq!(
        from_beve::<BTreeMap<String, u8>>(&to_beve_with::<SkipNull, _>(&AllOptional::default()))
            .unwrap(),
        BTreeMap::new()
    );
}

#[test]
fn a_map_is_indented_like_an_object() {
    let map = BTreeMap::from([("a", 1), ("b", 2)]);
    assert_eq!(
        to_string_with::<Pretty, _>(&map),
        "{\n  \"a\": 1,\n  \"b\": 2\n}"
    );
}

/// The pretty document has to be the same document, not merely a similar one.
#[test]
fn indenting_changes_nothing_a_reader_sees() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into(), "b".into()],
    };
    for text in [
        to_string(&value),
        to_string_with::<Pretty, _>(&value),
        to_string_with::<Config, _>(&value),
    ] {
        // `Config` drops the null, which reading restores as the default.
        assert_eq!(from_str::<Nest>(&text).unwrap(), value);
    }
}

/// A sink writer drains as it fills, and the pretty path is the one that pops
/// a byte back off the buffer to put the closing bracket on its own line. That
/// byte has to still be there at every buffer size.
#[test]
fn draining_does_not_disturb_the_closing_bracket() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["alpha".into(), "beta".into()],
    };
    let expected = to_string_with::<Pretty, _>(&value);
    for capacity in 1..48 {
        let mut out = Vec::new();
        structio::json::to_writer_buffered_with::<Pretty, _, _>(&value, &mut out, capacity)
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), expected, "at {capacity}");
    }
}

// ---------------------------------------------------------------------------
// NEW_LINES_IN_ARRAYS
// ---------------------------------------------------------------------------

/// The point of the setting: an array holds its elements on the line its
/// bracket sits on, and everything else is indented as before.
#[test]
fn an_inline_array_stays_on_one_line() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into(), "b".into()],
    };
    assert_eq!(
        to_string_with::<PrettyInlineArrays, _>(&value),
        "{\n  \"inner\": {\n    \"name\": \"t1\",\n    \"reading\": 21.5,\n    \
         \"note\": null\n  },\n  \"tags\": [\"a\", \"b\"]\n}"
    );
}

/// An array that writes no line of its own indents nothing against it, so an
/// object inside one is indented from the line the array began on rather than
/// from a level the array claimed and never used.
#[test]
fn an_inline_array_takes_no_level_of_indentation() {
    #[derive(Default)]
    struct Records {
        rows: Vec<Sensor>,
        grid: Vec<Vec<u8>>,
        empty: Vec<u8>,
    }
    structio::object!(Records { rows, grid, empty });

    let value = Records {
        rows: vec![sensor()],
        grid: vec![vec![1, 2], vec![3]],
        empty: Vec::new(),
    };
    assert_eq!(
        to_string_with::<WideInline, _>(&value),
        "{\n    \"rows\": [{\n        \"name\": \"t1\",\n        \"reading\": 21.5,\n        \
         \"note\": null\n    }],\n    \"grid\": [[1, 2], [3]],\n    \"empty\": []\n}"
    );
}

/// The shape `docs/options.md` puts on the page, which is prose until
/// something runs it. Two objects in one inline array is the case worth
/// pinning: the space after the comma lands against a `}` that is on a line of
/// its own, and it is the only place the two halves of the setting meet.
#[test]
fn an_object_in_an_inline_array_is_joined_after_the_comma() {
    #[derive(Default)]
    struct Row {
        name: String,
        count: u8,
    }
    structio::object!(Row { name, count });

    #[derive(Default)]
    struct Rows {
        rows: Vec<Row>,
        grid: Vec<Vec<u8>>,
    }
    structio::object!(Rows { rows, grid });

    let value = Rows {
        rows: vec![
            Row {
                name: "a".into(),
                count: 1,
            },
            Row {
                name: "b".into(),
                count: 2,
            },
        ],
        grid: vec![vec![1, 2], vec![3]],
    };
    assert_eq!(
        to_string_with::<PrettyInlineArrays, _>(&value),
        "{\n  \"rows\": [{\n    \"name\": \"a\",\n    \"count\": 1\n  }, {\n    \
         \"name\": \"b\",\n    \"count\": 2\n  }],\n  \"grid\": [[1, 2], [3]]\n}"
    );
}

/// The setting says where the line breaks go, and a document with no line
/// breaks at all has nowhere to put the difference. A stray space after a
/// comma would be the way this leaks into compact output.
#[test]
fn a_compact_policy_ignores_it() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into(), "b".into()],
    };
    assert_eq!(
        to_string_with::<CompactInline, _>(&value),
        to_string(&value)
    );
}

/// BEVE has no whitespace to place, so this is one more setting it does not
/// look at.
#[test]
fn beve_ignores_it_too() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into(), "b".into()],
    };
    assert_eq!(
        to_beve_with::<PrettyInlineArrays, _>(&value),
        to_beve(&value)
    );
}

/// Spacing is spacing: the same document comes back.
#[test]
fn an_inline_array_changes_nothing_a_reader_sees() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into(), "b".into()],
    };
    for text in [
        to_string_with::<PrettyInlineArrays, _>(&value),
        to_string_with::<WideInline, _>(&value),
        to_string_with::<CompactInline, _>(&value),
    ] {
        assert_eq!(from_str::<Nest>(&text).unwrap(), value);
    }
}

/// `Complex` and `Matrix` open and close their containers by hand, so they
/// have to be told about the setting by the same helpers everything else uses.
#[test]
fn the_hand_built_containers_go_inline_as_well() {
    use structio::{Complex, Matrix, MatrixLayout};

    let z = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, 4.0)];
    assert_eq!(
        to_string_with::<PrettyInlineArrays, _>(&z),
        "[[1, 2], [3, 4]]"
    );

    let m = Matrix::new(MatrixLayout::RowMajor, vec![2, 2], vec![1i32, 2, 3, 4]).unwrap();
    assert_eq!(
        to_string_with::<PrettyInlineArrays, _>(&m),
        "{\n  \"layout\": \"layout_right\",\n  \"extents\": [2, 2],\n  \
         \"value\": [1, 2, 3, 4]\n}"
    );
}

/// Every array shape in the transcoder has a loop of its own, and each has to
/// agree with what the writer would have produced from the value itself.
#[test]
fn every_transcoded_array_shape_stays_on_one_line() {
    use structio::{Complex, Matrix, MatrixLayout};

    for (doc, expected) in [
        (to_beve(&vec![1u8, 2]), "[1, 2]"),
        (to_beve(&vec![true, false]), "[true, false]"),
        (to_beve(&vec!["a", "b"]), "[\"a\", \"b\"]"),
        (to_beve(&Vec::<u8>::new()), "[]"),
        (to_beve(&(1u8, "a")), "[1, \"a\"]"),
        (to_beve(&Complex::new(1.0f64, 2.0)), "[1, 2]"),
        (to_beve(&vec![Complex::new(1.0f64, 2.0)]), "[[1, 2]]"),
    ] {
        assert_eq!(
            beve_to_json_with::<PrettyInlineArrays>(&doc).unwrap(),
            expected
        );
    }

    let m = Matrix::new(MatrixLayout::ColumnMajor, vec![1, 3], vec![7i32, 8, 9]).unwrap();
    assert_eq!(
        beve_to_json_with::<PrettyInlineArrays>(&to_beve(&m)).unwrap(),
        to_string_with::<PrettyInlineArrays, _>(&m)
    );
}

/// The closing bracket of an inline array overwrites the trailing comma the
/// way a compact one does, and a draining sink keeps exactly one byte back for
/// it. That byte has to still be there at every buffer size.
#[test]
fn draining_does_not_disturb_an_inline_arrays_bracket() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["alpha".into(), "beta".into()],
    };
    let expected = to_string_with::<PrettyInlineArrays, _>(&value);
    for capacity in 1..48 {
        let mut out = Vec::new();
        structio::json::to_writer_buffered_with::<PrettyInlineArrays, _, _>(
            &value, &mut out, capacity,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), expected, "at {capacity}");
    }
}

// ---------------------------------------------------------------------------
// SKIP_NULL
// ---------------------------------------------------------------------------

#[test]
fn a_none_member_is_left_out_of_both_formats() {
    let json = to_string_with::<SkipNull, _>(&sensor());
    assert_eq!(json, "{\"name\":\"t1\",\"reading\":21.5}");

    // BEVE states its member count before its members, so a count that did not
    // account for the skip would leave the reader inside the next value.
    // Transcoding walks the document with no schema to lean on, which is the
    // check that the stated count and the bytes agree.
    let doc = to_beve_with::<SkipNull, _>(&sensor());
    assert_eq!(structio::beve_to_json(&doc).unwrap(), json);
    assert_eq!(from_beve::<Sensor>(&doc).unwrap(), sensor());
}

#[test]
fn a_present_member_is_written_by_every_policy() {
    let value = Sensor {
        note: Some("ok".into()),
        ..sensor()
    };
    assert_eq!(
        to_string_with::<SkipNull, _>(&value),
        "{\"name\":\"t1\",\"reading\":21.5,\"note\":\"ok\"}"
    );
    assert_eq!(to_beve_with::<SkipNull, _>(&value), to_beve(&value));
    assert_eq!(
        from_beve::<Sensor>(&to_beve_with::<SkipNull, _>(&value)).unwrap(),
        value
    );
}

/// The absence is what reading treats as "leave the destination alone", so a
/// value skipped on the way out comes back as whatever `Default` gives.
#[test]
fn skipping_round_trips_through_the_default() {
    for doc in [to_beve_with::<SkipNull, _>(&sensor()), to_beve(&sensor())] {
        assert_eq!(from_beve::<Sensor>(&doc).unwrap(), sensor());
    }
    assert_eq!(
        from_str::<Sensor>(&to_string_with::<SkipNull, _>(&sensor())).unwrap(),
        sensor()
    );
}

/// Dropping a null from a sequence would shorten it and shift every index
/// after it, so the policy deliberately stops at object members.
#[test]
fn a_null_inside_a_sequence_is_still_written() {
    let items = vec![Some(1u8), None, Some(3)];
    assert_eq!(to_string_with::<SkipNull, _>(&items), "[1,null,3]");
    assert_eq!(
        from_beve::<Vec<Option<u8>>>(&to_beve_with::<SkipNull, _>(&items)).unwrap(),
        items
    );
}

/// A map's null value is data rather than an absent field, and its length is
/// not known until it has been walked.
#[test]
fn a_null_map_value_is_still_written() {
    let map = BTreeMap::from([("a", None::<u8>), ("b", Some(2))]);
    assert_eq!(to_string_with::<SkipNull, _>(&map), "{\"a\":null,\"b\":2}");
    assert_eq!(
        from_beve::<BTreeMap<String, Option<u8>>>(&to_beve_with::<SkipNull, _>(&map)).unwrap(),
        BTreeMap::from([("a".to_owned(), None), ("b".to_owned(), Some(2))])
    );
}

/// Absence is a property of what would be written, so a wrapper around `None`
/// is as absent as a bare one, and `Some(())` is absent because `()` is.
#[test]
fn the_wrappers_forward_their_emptiness() {
    #[derive(Default)]
    struct Wrapped {
        boxed: Box<Option<u8>>,
        unit: (),
        nested: Option<()>,
        present: Box<Option<u8>>,
    }
    structio::object!(Wrapped {
        boxed,
        unit,
        nested,
        present
    });

    let value = Wrapped {
        present: Box::new(Some(7)),
        ..Default::default()
    };
    assert_eq!(to_string_with::<SkipNull, _>(&value), "{\"present\":7}");
    let doc = to_beve_with::<SkipNull, _>(&value);
    assert_eq!(
        from_beve::<BTreeMap<String, u8>>(&doc).unwrap(),
        BTreeMap::from([("present".to_owned(), 7)])
    );
}

/// A nested struct is skipped only if the struct itself writes as null, which
/// no object does. An empty object is still an object.
#[test]
fn a_struct_member_is_never_absent() {
    let value = Nest {
        inner: sensor(),
        tags: Vec::new(),
    };
    assert_eq!(
        to_string_with::<SkipNull, _>(&value),
        "{\"inner\":{\"name\":\"t1\",\"reading\":21.5},\"tags\":[]}"
    );
}

// ---------------------------------------------------------------------------
// The BEVE member count
// ---------------------------------------------------------------------------

/// The count is written before the members, so a wrong one is not a document a
/// reader rejects: it is one where the reader takes the next value's bytes for
/// a member. A debug build asserts the two agree.
///
/// The check costs a counter on the member path, which is why it is not in a
/// release build, and why this test is not either.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "count_fields")]
fn a_count_that_disagrees_with_the_members_is_caught() {
    struct Liar;
    impl structio::Keys for Liar {
        const KEYS: &'static [&'static str] = &["a", "b"];
        const MAP: &'static structio::KeyMap = &structio::KeyMap::build(Self::KEYS);
    }
    impl beve::WriteObject for Liar {
        fn write_fields<O: Options>(&self, w: &mut beve::Writer<'_, O>) {
            w.member(&[1, b'a'], &1u8);
        }
        fn count_fields<O: Options>(&self) -> usize {
            // Claims both members, writes one.
            <Self as structio::Keys>::KEYS.len()
        }
    }
    impl beve::Write for Liar {
        fn write<O: Options>(&self, w: &mut beve::Writer<'_, O>) {
            w.write_object(self);
        }
    }
    to_beve(&Liar);
}

/// Nested objects share one counter, so the inner one must not leave the outer
/// one's tally behind it.
#[test]
fn nesting_does_not_confuse_the_member_check() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into()],
    };
    for doc in [to_beve(&value), to_beve_with::<SkipNull, _>(&value)] {
        assert_eq!(from_beve::<Nest>(&doc).unwrap(), value);
    }
}

// ---------------------------------------------------------------------------
// Where a policy does not reach
// ---------------------------------------------------------------------------

/// BEVE has no whitespace to put anywhere, so asking for indentation gets the
/// same bytes back.
#[test]
fn beve_ignores_pretty() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into()],
    };
    assert_eq!(to_beve_with::<Pretty, _>(&value), to_beve(&value));
    assert_eq!(to_beve_with::<Standard, _>(&value), to_beve(&value));
}

/// Transcoding has no schema to consult, which is exactly when the shape has
/// to come off the page.
#[test]
fn a_transcode_can_be_indented() {
    let value = Nest {
        inner: sensor(),
        tags: vec!["a".into()],
    };
    let doc = to_beve(&value);
    assert_eq!(
        beve_to_json_with::<Pretty>(&doc).unwrap(),
        to_string_with::<Pretty, _>(&value)
    );
    assert_eq!(
        beve_to_json_with::<Standard>(&doc).unwrap(),
        to_string(&value)
    );
}

/// Every typed array shape has its own loop in the transcoder, so each needs
/// its own line breaks.
#[test]
fn every_transcoded_array_shape_is_indented() {
    for (doc, expected) in [
        (to_beve(&vec![1u8, 2]), "[\n  1,\n  2\n]"),
        (to_beve(&vec![true, false]), "[\n  true,\n  false\n]"),
        (to_beve(&vec!["a", "b"]), "[\n  \"a\",\n  \"b\"\n]"),
        (to_beve(&Vec::<u8>::new()), "[]"),
    ] {
        assert_eq!(beve_to_json_with::<Pretty>(&doc).unwrap(), expected);
    }
}

/// Reading takes no policy at all, so a document written under one is read by
/// the ordinary entry points and an error still points at the right byte.
#[test]
fn reading_is_unaffected() {
    let text = to_string_with::<Pretty, _>(&sensor());
    let broken = text.replace("21.5", "nope");
    let err = from_str::<Sensor>(&broken).unwrap_err();
    assert_eq!(err.code, ErrorCode::ExpectedNumber);
    // The offset still points into the indented text, not into what a compact
    // document would have been.
    assert_eq!(&broken[err.index..err.index + 4], "nope");
}

/// The snippets in `docs/options.md`, which are prose until something runs
/// them.
#[test]
fn the_documented_examples_are_what_happens() {
    #[derive(Default)]
    struct Server {
        port: u16,
        tls: Option<String>,
    }
    structio::object!(Server { port, tls });

    let server = Server {
        port: 8080,
        tls: None,
    };

    assert_eq!(to_string(&server), r#"{"port":8080,"tls":null}"#);
    assert_eq!(
        to_string_with::<Pretty, _>(&server),
        "{\n  \"port\": 8080,\n  \"tls\": null\n}"
    );
    assert_eq!(to_string_with::<SkipNull, _>(&server), r#"{"port":8080}"#);

    // "the plain entry point is the `Standard` policy"
    assert_eq!(to_string(&server), to_string_with::<Standard, _>(&server));
    assert_eq!(to_beve(&server), to_beve_with::<Standard, _>(&server));
}

// ---------------------------------------------------------------------------
// The extension types, which build their containers by hand
// ---------------------------------------------------------------------------

/// `Complex` and `Matrix` write their JSON without going through `object!`, so
/// they open and close containers themselves. They once did that without
/// taking a level of indentation with them, which put every member of a matrix
/// against the wrong margin and made the direct write disagree with the
/// transcode of the same value.
#[test]
fn a_hand_built_container_is_indented_like_any_other() {
    use structio::{Complex, Matrix, MatrixLayout};

    let z = vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, 4.0)];
    assert_eq!(
        to_string_with::<Pretty, _>(&z),
        "[\n  [\n    1,\n    2\n  ],\n  [\n    3,\n    4\n  ]\n]"
    );

    let m = Matrix::new(MatrixLayout::RowMajor, vec![2, 2], vec![1i32, 2, 3, 4]).unwrap();
    assert_eq!(
        to_string_with::<Pretty, _>(&m),
        "{\n  \"layout\": \"layout_right\",\n  \"extents\": [\n    2,\n    2\n  ],\n  \
         \"value\": [\n    1,\n    2,\n    3,\n    4\n  ]\n}"
    );
}

/// The claim `ext/matrix.rs` makes about its two encodings: they differ in
/// compactness and in nothing else. The transcoder walks the document with no
/// schema, so agreeing with it is the check that both routes indent alike.
#[test]
fn the_two_routes_to_the_same_json_agree_under_every_policy() {
    use structio::{Complex, Matrix, MatrixLayout};

    #[derive(Default)]
    struct Holder {
        z: Vec<Complex<f64>>,
        m: Matrix<i32>,
        tag: Option<String>,
    }
    structio::object!(Holder { z, m, tag });

    let value = Holder {
        z: vec![Complex::new(1.0, -2.0)],
        m: Matrix::new(MatrixLayout::ColumnMajor, vec![1, 3], vec![7i32, 8, 9]).unwrap(),
        tag: None,
    };
    for doc in [to_beve(&value), to_beve_with::<Pretty, _>(&value)] {
        assert_eq!(
            beve_to_json_with::<Pretty>(&doc).unwrap(),
            to_string_with::<Pretty, _>(&value)
        );
        assert_eq!(structio::beve_to_json(&doc).unwrap(), to_string(&value));
    }
}

/// A lone complex number is the extension's other form, and takes no level of
/// its own where a run of them does.
#[test]
fn a_lone_complex_number_is_indented_where_it_sits() {
    use structio::Complex;

    let z = Complex::new(1.0f64, 2.0);
    assert_eq!(to_string_with::<Pretty, _>(&z), "[\n  1,\n  2\n]");
    assert_eq!(
        beve_to_json_with::<Pretty>(&to_beve(&z)).unwrap(),
        to_string_with::<Pretty, _>(&z)
    );
}

// ---------------------------------------------------------------------------
// ERROR_ON_UNKNOWN_KEYS
// ---------------------------------------------------------------------------

/// The default is on, in both formats, which is the one setting here whose
/// default differs from what the crate did before it existed.
#[test]
fn an_unknown_key_is_refused_by_default_in_both_formats() {
    let json = r#"{"name":"a","reading":1,"note":null,"extra":1}"#;
    assert_eq!(
        from_str::<Sensor>(json).unwrap_err().code,
        ErrorCode::UnknownKey
    );

    let doc = to_beve(&Extra {
        name: "a".into(),
        extra: 1,
    });
    assert_eq!(
        from_beve::<Sensor>(&doc).unwrap_err().code,
        ErrorCode::UnknownKey
    );
}

#[derive(Default, Debug, PartialEq)]
struct Extra {
    name: String,
    extra: u32,
}
structio::object!(Extra { name, extra });

#[test]
fn skip_unknown_steps_over_it_in_both_formats() {
    let json = r#"{"extra":[1,2,{"deep":null}],"name":"a","reading":1.5}"#;
    let got = from_str_with::<SkipUnknown, Sensor>(json).unwrap();
    assert_eq!(got.name, "a");
    assert_eq!(got.reading, 1.5);

    let doc = to_beve(&Extra {
        name: "a".into(),
        extra: 1,
    });
    let got = from_beve_with::<SkipUnknown, Sensor>(&doc).unwrap();
    assert_eq!(got.name, "a");
}

/// The error names the key, so the position has to be the key's first byte:
/// not the brace before it, and not the value it would have introduced.
#[test]
fn the_error_is_located_at_the_key() {
    let json = r#"{"name":"a","typo":1}"#;
    let err = from_str::<Sensor>(json).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownKey);
    assert_eq!(&json[err.index..err.index + 4], "typo");
}

/// Same promise on the BEVE side, where it takes more doing: the key's bytes
/// have already been consumed by the time anything knows the key is unknown,
/// so the position has to be wound back to them.
#[test]
fn the_beve_error_is_located_at_the_key_too() {
    let doc = to_beve(&Extra {
        name: "a".into(),
        extra: 1,
    });
    let err = from_beve::<Sensor>(&doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownKey);
    assert_eq!(&doc[err.index..err.index + 5], b"extra");
}

/// A nested object gets the policy too. It arrives through the parser's type
/// rather than through an argument, so there is nothing for an inner struct to
/// forget to pass on.
#[test]
fn the_policy_reaches_a_nested_object() {
    let json = r#"{"inner":{"name":"a","reading":1,"typo":2},"tags":[]}"#;
    assert_eq!(
        from_str::<Nest>(json).unwrap_err().code,
        ErrorCode::UnknownKey
    );
    assert!(from_str_with::<SkipUnknown, Nest>(json).is_ok());
}

/// A map claims every key it is given, so none of them is ever unknown. This
/// is the case that would break if the check lived in the key lookup rather
/// than in the object reader.
#[test]
fn a_map_has_no_unknown_keys() {
    let json = r#"{"a":1,"b":2,"zzz":3}"#;
    let got = from_str::<BTreeMap<String, u32>>(json).unwrap();
    assert_eq!(got.len(), 3);

    let doc = to_beve(&got);
    assert_eq!(from_beve::<BTreeMap<String, u32>>(&doc).unwrap(), got);
}

/// A positional struct has no keys at all, so the setting has nothing to say
/// about it either way: a wrong length is still a length error.
#[test]
fn a_positional_struct_is_unaffected() {
    #[derive(Default, Debug, PartialEq)]
    struct Point {
        x: i32,
        y: i32,
    }
    structio::array!(Point [x, y]);

    assert_eq!(from_str::<Point>("[1,2]").unwrap(), Point { x: 1, y: 2 });
    assert_eq!(
        from_str::<Point>("[1,2,3]").unwrap_err().code,
        ErrorCode::ArrayLengthMismatch
    );
}

/// The value under an unknown key is stepped over rather than parsed, so a
/// document that is malformed inside one is accepted under `SkipUnknown` and
/// refused under the default. The two answers differ for the same bytes, which
/// is the point: refusing costs nothing and looks at less.
#[test]
fn a_malformed_value_under_an_unknown_key() {
    let json = r#"{"junk":1.2.3e--,"name":"a"}"#;
    assert!(from_str_with::<SkipUnknown, Sensor>(json).is_ok());
    assert_eq!(
        from_str::<Sensor>(json).unwrap_err().code,
        ErrorCode::UnknownKey
    );
}

/// A repeated key is not an unknown one: the second occurrence matches the
/// same field and wins, exactly as it did before.
#[test]
fn a_repeated_key_is_still_the_last_one() {
    let got = from_str::<Sensor>(r#"{"name":"a","name":"b","reading":1,"note":null}"#).unwrap();
    assert_eq!(got.name, "b");
}

/// The policy is set once on the stream and every value read through it
/// follows, which is the whole reason it sits on the type rather than on the
/// call.
#[test]
fn a_stream_carries_the_policy() {
    let ndjson =
        b"{\"name\":\"a\",\"reading\":1,\"extra\":0}\n{\"name\":\"b\",\"reading\":2,\"extra\":0}";

    let mut strict = Documents::lines(&ndjson[..]);
    assert!(strict.iter::<Sensor>().next().unwrap().is_err());

    let mut lenient = Documents::lines(&ndjson[..]).with_options::<SkipUnknown>();
    let names: Vec<String> = lenient.iter::<Sensor>().map(|r| r.unwrap().name).collect();
    assert_eq!(names, ["a", "b"]);
}

/// Reading a value the pointer names is one thing; walking to it is another.
/// Nothing on the way is read against a schema, so a sibling key nothing
/// claims is not an unknown key to anyone.
#[test]
fn a_pointer_walks_past_keys_no_type_claims() {
    let doc = to_beve(&Nest {
        inner: Sensor {
            name: "a".into(),
            reading: 1.5,
            note: None,
        },
        tags: vec!["x".into()],
    });
    assert_eq!(
        structio::from_beve_at::<f64>(&doc, "/inner/reading").unwrap(),
        1.5
    );
}

/// `Matrix` reads a fixed set of three keys, so a fourth is as unknown as any
/// other. It is the one type here whose reader is hand written rather than
/// generated, which is exactly why it is worth pinning: it has to opt into the
/// policy by hand, and nothing makes it.
#[test]
fn a_matrix_refuses_a_member_its_shape_does_not_name() {
    use structio::Matrix;

    let json = r#"{"layout":"layout_right","extents":[2,2],"value":[1,2,3,4],"bogus":7}"#;
    assert_eq!(
        from_str::<Matrix<f64>>(json).unwrap_err().code,
        ErrorCode::UnknownKey
    );
    assert!(from_str_with::<SkipUnknown, Matrix<f64>>(json).is_ok());

    // The BEVE object form, which is what a producer without the extension
    // writes. The extension form carries no keys at all and is unaffected.
    #[derive(Default)]
    struct Doc {
        layout: String,
        extents: Vec<u64>,
        value: Vec<f64>,
        bogus: u32,
    }
    structio::object!(Doc {
        layout,
        extents,
        value,
        bogus
    });

    let doc = to_beve(&Doc {
        layout: "layout_right".into(),
        extents: vec![2, 2],
        value: vec![1.0, 2.0, 3.0, 4.0],
        bogus: 7,
    });
    assert_eq!(
        from_beve::<Matrix<f64>>(&doc).unwrap_err().code,
        ErrorCode::UnknownKey
    );
    assert!(from_beve_with::<SkipUnknown, Matrix<f64>>(&doc).is_ok());
}

/// A key that runs off the end of the input is not a key nothing claims: it is
/// a document that stopped early. `match_key` cannot tell the two apart, since
/// it fails the same way for both, so the object reader has to.
#[test]
fn a_truncated_key_is_not_an_unknown_one() {
    for doc in [r#"{"na"#, r#"{"name":"a","rea"#] {
        assert_eq!(
            from_str::<Sensor>(doc).unwrap_err().code,
            ErrorCode::UnexpectedEnd,
            "for {doc:?}"
        );
        // The lenient policy has always said so, and the strict one must not
        // give the worse diagnostic of the two.
        assert_eq!(
            from_str_with::<SkipUnknown, Sensor>(doc).unwrap_err().code,
            ErrorCode::UnexpectedEnd,
            "for {doc:?}"
        );
    }
}

/// The BEVE rewind has to land on the key of the object that holds it, not on
/// anything in the object that encloses that one.
#[test]
fn the_beve_error_is_located_at_a_nested_key() {
    #[derive(Default)]
    struct Outer {
        inner: Extra,
    }
    structio::object!(Outer { inner });

    let doc = to_beve(&Outer {
        inner: Extra {
            name: "a".into(),
            extra: 1,
        },
    });
    // `Nest`'s inner is a `Sensor`, which does not claim `extra`.
    let err = from_beve::<Nest>(&doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnknownKey);
    assert_eq!(&doc[err.index..err.index + 5], b"extra");
}

/// `Feed` takes the policy the same way `Documents` does, in both formats.
#[test]
fn a_feed_carries_the_policy() {
    let json = br#"{"name":"a","reading":1,"extra":0}"#;

    let mut strict = structio::Feed::values();
    strict.push(json);
    strict.end();
    assert!(strict.next_value::<Sensor>().unwrap().is_err());

    let mut lenient = structio::Feed::values().with_options::<SkipUnknown>();
    lenient.push(json);
    lenient.end();
    assert_eq!(lenient.next_value::<Sensor>().unwrap().unwrap().name, "a");

    let doc = to_beve(&Extra {
        name: "a".into(),
        extra: 1,
    });
    let mut strict = beve::Feed::values();
    strict.push(&doc);
    strict.end();
    assert!(strict.next_value::<Sensor>().unwrap().is_err());

    let mut lenient = beve::Feed::values().with_options::<SkipUnknown>();
    lenient.push(&doc);
    lenient.end();
    assert_eq!(lenient.next_value::<Sensor>().unwrap().unwrap().name, "a");
}

/// The walk to a pointer's target ignores the policy, which the test above
/// pins. This is the other half: the target itself does not.
#[test]
fn the_policy_governs_a_pointers_target() {
    #[derive(Default)]
    struct Outer {
        inner: Sensor,
    }
    structio::object!(Outer { inner });

    let doc = to_beve(&Outer { inner: sensor() });
    assert_eq!(
        structio::from_beve_at::<Extra>(&doc, "/inner")
            .unwrap_err()
            .code,
        ErrorCode::UnknownKey
    );
    assert_eq!(
        structio::from_beve_at_with::<SkipUnknown, Extra>(&doc, "/inner")
            .unwrap()
            .name,
        sensor().name
    );
}

/// A destination that borrows from the input takes the policy like any other.
#[test]
fn a_borrowed_destination_takes_the_policy() {
    #[derive(Default, Debug)]
    struct Borrowed<'de> {
        name: &'de str,
    }
    structio::object!(['de] Borrowed<'de> { name });

    let json = r#"{"name":"a","extra":1}"#;
    assert_eq!(
        from_str::<Borrowed>(json).unwrap_err().code,
        ErrorCode::UnknownKey
    );
    assert_eq!(
        from_str_with::<SkipUnknown, Borrowed>(json).unwrap().name,
        "a"
    );
}

/// A positional struct reached through an object member: the array is still
/// judged by its length, and the object around it by its keys.
#[test]
fn a_positional_struct_inside_an_object() {
    #[derive(Default, Debug, PartialEq)]
    struct Point {
        x: i32,
        y: i32,
    }
    structio::array!(Point [x, y]);

    #[derive(Default, Debug, PartialEq)]
    struct Holder {
        at: Point,
    }
    structio::object!(Holder { at });

    assert_eq!(
        from_str::<Holder>(r#"{"at":[1,2]}"#).unwrap().at,
        Point { x: 1, y: 2 }
    );
    assert_eq!(
        from_str::<Holder>(r#"{"at":[1,2,3]}"#).unwrap_err().code,
        ErrorCode::ArrayLengthMismatch
    );
    assert_eq!(
        from_str::<Holder>(r#"{"at":[1,2],"z":0}"#)
            .unwrap_err()
            .code,
        ErrorCode::UnknownKey
    );
}

// ---------------------------------------------------------------------------
// ERROR_ON_MISSING_KEYS
// ---------------------------------------------------------------------------

/// Everything `Sensor` declares except the last member, so a document written
/// from it is one a `Sensor` can read and is missing a key while doing so.
#[derive(Default, Debug, PartialEq)]
struct Partial {
    name: String,
    reading: f64,
}
structio::object!(Partial { name, reading });

/// Unknown keys stepped over, declared ones still required: a document that
/// has to say at least what the schema says, and may say more.
#[derive(Clone, Copy)]
struct Superset;

impl Options for Superset {
    const ERROR_ON_UNKNOWN_KEYS: bool = false;
    const ERROR_ON_MISSING_KEYS: bool = true;
}

fn partial() -> Partial {
    Partial {
        name: "a".into(),
        reading: 1.5,
    }
}

/// The default is off, which is what makes reading a merge: the member the
/// document did not mention keeps whatever the destination already held.
#[test]
fn a_missing_key_is_accepted_by_default_in_both_formats() {
    let json = r#"{"name":"a","reading":1.5}"#;
    let got = from_str::<Sensor>(json).unwrap();
    assert_eq!(got.name, "a");
    assert_eq!(got.note, None);

    let doc = to_beve(&partial());
    assert_eq!(from_beve::<Sensor>(&doc).unwrap().name, "a");
}

#[test]
fn require_keys_refuses_it_in_both_formats() {
    let json = r#"{"name":"a","reading":1.5}"#;
    assert_eq!(
        from_str_with::<RequireKeys, Sensor>(json).unwrap_err().code,
        ErrorCode::MissingKey
    );

    let doc = to_beve(&partial());
    assert_eq!(
        from_beve_with::<RequireKeys, Sensor>(&doc)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );
}

/// A complete document satisfies it whatever order the members arrive in: the
/// check is a set, not a sequence.
#[test]
fn every_key_present_in_any_order_satisfies_it() {
    for json in [
        r#"{"name":"a","reading":1.5,"note":null}"#,
        r#"{"note":"n","name":"a","reading":1.5}"#,
        r#"{"reading":1.5,"note":null,"name":"a"}"#,
    ] {
        assert!(
            from_str_with::<RequireKeys, Sensor>(json).is_ok(),
            "for {json:?}"
        );
    }

    let doc = to_beve(&sensor());
    assert_eq!(
        from_beve_with::<RequireKeys, Sensor>(&doc).unwrap(),
        sensor()
    );
}

/// An empty object is the case the loop never runs for, so it is the one an
/// implementation is most likely to let through.
#[test]
fn an_empty_object_is_missing_every_key() {
    assert!(from_str::<Sensor>("{}").is_ok());
    assert_eq!(
        from_str_with::<RequireKeys, Sensor>("{}").unwrap_err().code,
        ErrorCode::MissingKey
    );
}

/// The object is what is incomplete, so the position is the brace that opened
/// it rather than the byte that closed it.
#[test]
fn the_missing_key_error_is_located_at_the_object() {
    let json = r#"{"name":"a","reading":1.5}"#;
    let err = from_str_with::<RequireKeys, Sensor>(json).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    assert_eq!(err.index, 0);

    // And on a nested one, the inner brace rather than the outer. A nested
    // object gets the policy through the parser's type, so there is nothing
    // for an inner struct to forget to pass on.
    let json = r#"{"inner":{"name":"a","reading":1.5},"tags":[]}"#;
    assert!(from_str::<Nest>(json).is_ok());
    let err = from_str_with::<RequireKeys, Nest>(json).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    assert_eq!(err.index, json.find(r#"{"name""#).unwrap());
}

/// The same promise on the BEVE side, where the cursor has to be wound back
/// past the members it already walked.
#[test]
fn the_beve_missing_key_error_is_located_at_the_object() {
    let doc = to_beve(&partial());
    let err = from_beve_with::<RequireKeys, Sensor>(&doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    assert_eq!(err.index, 0);

    #[derive(Default)]
    struct Outer {
        inner: Partial,
    }
    structio::object!(Outer { inner });

    #[derive(Default, Debug)]
    struct Wrapper {
        inner: Sensor,
    }
    structio::object!(Wrapper { inner });

    let doc = to_beve(&Outer { inner: partial() });
    let err = from_beve_with::<RequireKeys, Wrapper>(&doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    // Not the outer object, and still an object header: the same one the
    // document opens with, both being string-keyed objects.
    assert!(err.index > 0);
    assert_eq!(doc[err.index], doc[0]);
}

/// A map declares nothing, so there is nothing it can be missing. This is the
/// counterpart of the unknown-key case: neither setting has anything to say
/// about a container whose keys are data.
#[test]
fn a_map_is_never_missing_a_key() {
    let json = r#"{"a":1}"#;
    assert_eq!(
        from_str_with::<RequireKeys, BTreeMap<String, u32>>(json)
            .unwrap()
            .len(),
        1
    );
    assert!(
        from_str_with::<RequireKeys, BTreeMap<String, u32>>("{}")
            .unwrap()
            .is_empty()
    );
}

/// A positional struct is judged by its length and always was, so the setting
/// changes nothing: a short array is an `ArrayLengthMismatch` under either
/// policy rather than a missing key under one of them.
#[test]
fn a_positional_struct_is_unaffected_by_missing_keys() {
    #[derive(Default, Debug, PartialEq)]
    struct Point {
        x: i32,
        y: i32,
    }
    structio::array!(Point [x, y]);

    assert_eq!(
        from_str_with::<RequireKeys, Point>("[1,2]").unwrap(),
        Point { x: 1, y: 2 }
    );
    assert_eq!(
        from_str_with::<RequireKeys, Point>("[1]").unwrap_err().code,
        ErrorCode::ArrayLengthMismatch
    );
}

/// An `Option` field is not exempt. The test is whether the member is present,
/// not what it holds, so `null` satisfies it and absence does not: writing
/// under `SkipNull` and reading under `RequireKeys` contradict each other by
/// construction, and the round trip fails rather than quietly losing the
/// distinction.
#[test]
fn an_optional_field_is_still_required_to_be_present() {
    let with_null = to_string(&sensor());
    assert!(from_str_with::<RequireKeys, Sensor>(&with_null).is_ok());

    let dropped = to_string_with::<SkipNull, _>(&sensor());
    assert_eq!(
        from_str_with::<RequireKeys, Sensor>(&dropped)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );
}

/// A key repeated is one field filled twice, not two fields filled. Counting
/// members rather than tracking which ones would accept this document.
#[test]
fn a_repeated_key_does_not_stand_in_for_a_missing_one() {
    let json = r#"{"name":"a","name":"b","reading":1.5}"#;
    assert_eq!(
        from_str_with::<RequireKeys, Sensor>(json).unwrap_err().code,
        ErrorCode::MissingKey
    );
}

/// A member no field claims does not fill one either, and it is noticed first:
/// the unknown key is refused where it sits, before the object has ended.
#[test]
fn an_unknown_key_is_refused_before_a_missing_one_is_noticed() {
    let json = r#"{"name":"a","reading":1.5,"typo":1}"#;
    assert_eq!(
        from_str_with::<RequireKeys, Sensor>(json).unwrap_err().code,
        ErrorCode::UnknownKey
    );
}

/// The two settings are independent, and the combination neither built-in
/// policy provides is the useful one: say at least what the schema says, and
/// say more if you like.
#[test]
fn the_two_settings_compose() {
    let extra = r#"{"name":"a","reading":1.5,"note":null,"added":1}"#;
    assert!(from_str_with::<Superset, Sensor>(extra).is_ok());

    let short = r#"{"name":"a","reading":1.5,"added":1}"#;
    assert_eq!(
        from_str_with::<Superset, Sensor>(short).unwrap_err().code,
        ErrorCode::MissingKey
    );
}

/// Reading into a value that already holds data is a merge, and `RequireKeys`
/// is the option that refuses to merge: a patch is exactly a document that
/// leaves members out.
#[test]
fn require_keys_and_reading_into_pull_against_each_other() {
    let mut sensor = sensor();
    structio::read_into(&mut sensor, r#"{"reading":2.5}"#).unwrap();
    assert_eq!(sensor.name, "t1");
    assert_eq!(sensor.reading, 2.5);

    assert_eq!(
        structio::read_into_with::<RequireKeys, _>(&mut sensor, r#"{"reading":3.5}"#)
            .unwrap_err()
            .code,
        ErrorCode::MissingKey
    );
}

/// A stream sets the policy once and every value read through it follows.
#[test]
fn a_stream_carries_the_missing_key_policy() {
    let ndjson = b"{\"name\":\"a\",\"reading\":1}\n{\"name\":\"b\",\"reading\":2}";

    let mut lenient = Documents::lines(&ndjson[..]);
    assert_eq!(lenient.iter::<Sensor>().filter(|r| r.is_ok()).count(), 2);

    let mut strict = Documents::lines(&ndjson[..]).with_options::<RequireKeys>();
    assert!(strict.iter::<Sensor>().next().unwrap().is_err());

    let doc = to_beve(&partial());
    let mut strict = beve::Feed::values().with_options::<RequireKeys>();
    strict.push(&doc);
    strict.end();
    assert!(strict.next_value::<Sensor>().unwrap().is_err());
}

/// `Matrix` reads a fixed set of three keys through `read_map`, which knows
/// nothing about schemas, so it has to apply the policy by hand. Nothing makes
/// it, which is why this is pinned.
#[test]
fn a_matrix_requires_every_member_its_shape_names() {
    use structio::Matrix;

    let full = r#"{"layout":"layout_right","extents":[2,2],"value":[1,2,3,4]}"#;
    assert!(from_str_with::<RequireKeys, Matrix<f64>>(full).is_ok());

    // No layout, which is the member whose absence nothing else would catch:
    // the extents still describe the data, so the shape check passes.
    let short = r#"{"extents":[2,2],"value":[1,2,3,4]}"#;
    assert!(from_str::<Matrix<f64>>(short).is_ok());
    let err = from_str_with::<RequireKeys, Matrix<f64>>(short).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    // Against the object, as a generated reader reports it, rather than the
    // byte past the one that closed it.
    assert_eq!(err.index, 0);

    #[derive(Default)]
    struct Doc {
        extents: Vec<u64>,
        value: Vec<f64>,
    }
    structio::object!(Doc { extents, value });

    let doc = to_beve(&Doc {
        extents: vec![2, 2],
        value: vec![1.0, 2.0, 3.0, 4.0],
    });
    assert!(from_beve::<Matrix<f64>>(&doc).is_ok());
    let err = from_beve_with::<RequireKeys, Matrix<f64>>(&doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    assert_eq!(err.index, 0);

    // The BEVE extension form carries all three by construction and never has
    // a key to be missing.
    let m = Matrix::new(
        structio::MatrixLayout::RowMajor,
        vec![2, 2],
        vec![1.0f64, 2.0, 3.0, 4.0],
    )
    .unwrap();
    let doc = to_beve(&m);
    assert!(from_beve_with::<RequireKeys, Matrix<f64>>(&doc).is_ok());
}

/// The escape hatch, driven from outside the crate: a hand-written reader over
/// `read_map` applies the policy itself and reports against the object, which
/// takes `position` to remember where the object began and `rewind` to go back
/// to it once the gap is known. `Matrix` does exactly this, and nothing
/// outside the crate could unless both are public.
#[test]
fn a_hand_written_reader_can_report_against_the_object() {
    #[derive(Default, Debug)]
    struct Span {
        lo: u32,
        hi: u32,
    }

    impl<'de> structio::json::Read<'de> for Span {
        fn read<O: Options>(
            &mut self,
            p: &mut structio::json::Parser<'de, O>,
        ) -> Result<(), ErrorCode> {
            p.skip_ws();
            let open = p.position();
            let mut seen = 0u8;
            p.read_map(|p, key| match key.as_str() {
                "lo" => {
                    seen |= 1;
                    structio::json::Read::read(&mut self.lo, p)
                }
                "hi" => {
                    seen |= 2;
                    structio::json::Read::read(&mut self.hi, p)
                }
                _ if O::ERROR_ON_UNKNOWN_KEYS => Err(ErrorCode::UnknownKey),
                _ => p.skip_value(),
            })?;
            if O::ERROR_ON_MISSING_KEYS && seen != 0b11 {
                p.rewind(open);
                return Err(ErrorCode::MissingKey);
            }
            Ok(())
        }
    }

    let json = r#"  {"lo":1}"#;
    assert_eq!(from_str::<Span>(json).unwrap().hi, 0);

    let err = from_str_with::<RequireKeys, Span>(json).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingKey);
    // The brace, not the byte past the one that closed the object.
    assert_eq!(err.index, 2);
    assert_eq!(json.as_bytes()[err.index], b'{');
}

macro_rules! wide {
    ($name:ident { $($f:ident),* }) => {
        #[derive(Default, Debug)]
        struct $name { $($f: u8,)* }
        structio::object!($name { $($f),* });
    };
}

wide!(Wide {
    f00,
    f01,
    f02,
    f03,
    f04,
    f05,
    f06,
    f07,
    f08,
    f09,
    f10,
    f11,
    f12,
    f13,
    f14,
    f15,
    f16,
    f17,
    f18,
    f19,
    f20,
    f21,
    f22,
    f23,
    f24,
    f25,
    f26,
    f27,
    f28,
    f29,
    f30,
    f31,
    f32,
    f33,
    f34,
    f35,
    f36,
    f37,
    f38,
    f39,
    f40,
    f41,
    f42,
    f43,
    f44,
    f45,
    f46,
    f47,
    f48,
    f49,
    f50,
    f51,
    f52,
    f53,
    f54,
    f55,
    f56,
    f57,
    f58,
    f59,
    f60,
    f61,
    f62,
    f63
});

wide!(Wider {
    g00,
    g01,
    g02,
    g03,
    g04,
    g05,
    g06,
    g07,
    g08,
    g09,
    g10,
    g11,
    g12,
    g13,
    g14,
    g15,
    g16,
    g17,
    g18,
    g19,
    g20,
    g21,
    g22,
    g23,
    g24,
    g25,
    g26,
    g27,
    g28,
    g29,
    g30,
    g31,
    g32,
    g33,
    g34,
    g35,
    g36,
    g37,
    g38,
    g39,
    g40,
    g41,
    g42,
    g43,
    g44,
    g45,
    g46,
    g47,
    g48,
    g49,
    g50,
    g51,
    g52,
    g53,
    g54,
    g55,
    g56,
    g57,
    g58,
    g59,
    g60,
    g61,
    g62,
    g63,
    g64
});

/// Sixty-four fields is the widest struct the mask can hold, so it is the one
/// where the last field takes the top bit and the full mask is every bit set.
/// Both are off-by-one waiting to happen, and the shift that builds the mask
/// would overflow if the count were reached by the wrong arm.
#[test]
fn the_mask_holds_sixty_four_fields() {
    let full = to_string(&Wide::default());
    assert!(from_str_with::<RequireKeys, Wide>(&full).is_ok());

    for (member, leaving) in [(r#"{"f00":0,"#, "{"), (r#","f63":0"#, "")] {
        let short = full.replacen(member, leaving, 1);
        assert_ne!(short, full);
        assert_eq!(
            from_str_with::<RequireKeys, Wide>(&short).unwrap_err().code,
            ErrorCode::MissingKey,
            "without {member:?}"
        );
    }
}

/// The cap belongs to the option, not to the struct: sixty-five fields is a
/// compile error under `RequireKeys` and entirely ordinary under everything
/// else. That is what writing the assertion against the policy buys, and it is
/// what a tidier-looking `const` block inside the `if` would silently take
/// away, a constant being evaluated whether or not its branch is taken. This
/// test fails by not compiling.
#[test]
fn a_struct_too_wide_for_the_mask_reads_under_every_other_policy() {
    let json = to_string(&Wider::default());
    assert!(from_str::<Wider>(&json).is_ok());
    assert!(from_str_with::<SkipUnknown, Wider>(&json).is_ok());

    let doc = to_beve(&Wider::default());
    assert!(from_beve::<Wider>(&doc).is_ok());
    assert!(from_beve_with::<SkipUnknown, Wider>(&doc).is_ok());
}

// ---------------------------------------------------------------------------
// ALLOW_COMMENTS
// ---------------------------------------------------------------------------

/// One field, so a document can be mostly comment.
#[derive(Default, Debug, PartialEq)]
struct One {
    a: i32,
}
structio::object!(One { a });

/// Comments read, and every declared field still required: the combination a
/// hand-maintained configuration file wants.
#[derive(Clone, Copy)]
struct StrictJsonc;

impl Options for StrictJsonc {
    const ALLOW_COMMENTS: bool = true;
    const ERROR_ON_MISSING_KEYS: bool = true;
}

#[test]
fn a_comment_is_refused_by_default() {
    assert!(from_str::<One>("{\"a\":1} // done").is_err());
    assert!(from_str::<One>("/* lead */ {\"a\":1}").is_err());
    assert!(from_str::<One>("{\"a\": /* mid */ 1}").is_err());
}

/// Wherever whitespace may go, so may a comment.
#[test]
fn a_comment_goes_where_whitespace_goes() {
    let doc = r#"
        // leading
        { /* after the brace */
            "name" /* before the colon */ : /* after it */ "a",
            "reading": 1.5 // before the comma
            ,
            "note": null
            /* before the closing brace */
        } // trailing
    "#;

    let got = from_str_with::<AllowComments, Sensor>(doc).unwrap();
    assert_eq!(got.name, "a");
    assert_eq!(got.reading, 1.5);
}

/// Inside an array, inside a map, and inside a nested object: the whitespace
/// skipper is one function, so all of them come along at once.
#[test]
fn a_comment_goes_inside_every_container() {
    let doc = r#"[ /* a */ 1, // b
        2 /* c */ ]"#;
    assert_eq!(
        from_str_with::<AllowComments, Vec<i32>>(doc).unwrap(),
        [1, 2]
    );

    let doc = r#"{ // entries
        "x": 1, /* and */ "y": 2 }"#;
    let map = from_str_with::<AllowComments, BTreeMap<String, i32>>(doc).unwrap();
    assert_eq!(map.len(), 2);
}

/// A line comment ends at the newline, and at the end of the input when there
/// is no newline left to end it.
#[test]
fn a_line_comment_ends_at_the_line() {
    assert_eq!(
        from_str_with::<AllowComments, One>("// x\n{\"a\":1}")
            .unwrap()
            .a,
        1
    );
    assert_eq!(
        from_str_with::<AllowComments, One>("{\"a\":1} // x")
            .unwrap()
            .a,
        1
    );
    // Without the newline the value would be inside the comment.
    assert!(from_str_with::<AllowComments, One>("// x {\"a\":1}").is_err());
}

/// A comment is opaque. Whatever a scanner would otherwise make of the bytes
/// in it, they are not structure.
#[test]
fn a_comment_may_hold_anything() {
    let doc = r#"{ /* } ] " \ */ "a": 1 // } and " again
    }"#;
    assert_eq!(from_str_with::<AllowComments, One>(doc).unwrap().a, 1);
}

/// A block comment ends at the first `*/`. They do not nest, which is what
/// JSONC, JSON5, and C all say.
#[test]
fn block_comments_do_not_nest() {
    assert_eq!(
        from_str_with::<AllowComments, One>("/* /* */ {\"a\":1}")
            .unwrap()
            .a,
        1
    );
    // The second `*/` is therefore left over, and is trailing content.
    assert!(from_str_with::<AllowComments, One>("/* /* */ {\"a\":1} */").is_err());

    // The degenerate ones still close.
    assert_eq!(
        from_str_with::<AllowComments, One>("/**/{\"a\":1}")
            .unwrap()
            .a,
        1
    );
    assert_eq!(
        from_str_with::<AllowComments, One>("/***/{\"a\":1}")
            .unwrap()
            .a,
        1
    );
}

/// `//` in a string is two characters, and always was.
#[test]
fn a_string_is_not_commented() {
    let got = from_str_with::<AllowComments, Sensor>(
        r#"{"name":"http://x/*y*/z","reading":0,"note":null}"#,
    )
    .unwrap();
    assert_eq!(got.name, "http://x/*y*/z");
}

/// An incomplete comment is not consumed, so the error lands on the `/` that
/// began it rather than at the end of the document.
#[test]
fn an_incomplete_comment_is_reported_where_it_starts() {
    let doc = r#"{"a":1 /* never closed"#;
    let err = from_str_with::<AllowComments, One>(doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::ExpectedComma);
    assert_eq!(err.index, doc.find("/*").unwrap());

    // Past the value, the same comment is trailing content.
    let doc = r#"{"a":1} /* never closed"#;
    let err = from_str_with::<AllowComments, One>(doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::TrailingContent);
    assert_eq!(err.index, doc.find("/*").unwrap());

    // A `/` that begins nothing at all is refused the same way.
    let doc = r#"{"a":1} /x"#;
    let err = from_str_with::<AllowComments, One>(doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::TrailingContent);
    assert_eq!(err.index, doc.find("/x").unwrap());

    // Including where a colon was owed.
    let doc = r#"{"a" /x 1}"#;
    let err = from_str_with::<AllowComments, One>(doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::ExpectedColon);
    assert_eq!(err.index, doc.find("/x").unwrap());
}

/// The setting composes with the others rather than replacing them: a
/// commented document is still held to the key policies.
#[test]
fn comments_compose_with_the_key_settings() {
    let doc = r#"{ "a": 1 /* fine */, "b": 2 // unknown
    }"#;
    assert_eq!(
        from_str_with::<AllowComments, One>(doc).unwrap_err().code,
        ErrorCode::UnknownKey
    );

    let doc = r#"{ // only one of the two
        "name": "a" }"#;
    assert_eq!(
        from_str_with::<StrictJsonc, Sensor>(doc).unwrap_err().code,
        ErrorCode::MissingKey
    );
}

/// Nothing writes a comment, and BEVE has nowhere to put one, so the setting
/// is invisible to both.
#[test]
fn nothing_is_written_and_beve_is_unaffected() {
    let sensor = Sensor {
        name: "a".into(),
        reading: 1.5,
        note: None,
    };
    assert_eq!(
        to_string_with::<AllowComments, _>(&sensor),
        to_string(&sensor)
    );

    let doc = to_beve_with::<AllowComments, _>(&sensor);
    assert_eq!(doc, to_beve(&sensor));
    assert_eq!(
        from_beve_with::<AllowComments, Sensor>(&doc).unwrap(),
        sensor
    );
}

/// A comment reaches everything that reads through the parser, including the
/// hand-written readers.
#[test]
fn a_matrix_reads_with_comments() {
    use structio::Matrix;

    let doc = r#"{
        "layout": "layout_right", // row major
        "extents": [2, 2],
        "value": [1, 2, 3, 4] /* four of them */
    }"#;
    let m = from_str_with::<AllowComments, Matrix<f64>>(doc).unwrap();
    assert_eq!(m.extents(), [2, 2]);
}

/// The splitter divides a stream before the parser sees any of it, so it has
/// to know about comments too: this one holds a brace that would otherwise
/// close the object early.
#[test]
fn a_stream_splits_around_comments() {
    let input = b"// header\n{\"a\":1 /* } */}\n/* between */\n{\"a\":2}\n// trailer";

    let mut docs = Documents::values(&input[..]).with_options::<AllowComments>();
    let got: Vec<_> = docs.iter::<One>().map(|r| r.unwrap().a).collect();
    assert_eq!(got, [1, 2]);

    // Without the policy the same bytes are not a stream at all.
    let mut docs = Documents::values(&input[..]);
    assert!(docs.iter::<One>().next().unwrap().is_err());
}

/// An array stream, where the commas and the brackets are the splitter's own.
#[test]
fn an_array_stream_splits_around_comments() {
    let input = b"[ /* open */ {\"a\":1} // one\n, {\"a\":2} /* two */ ]";
    let mut docs = Documents::array(&input[..]).with_options::<AllowComments>();
    let got: Vec<_> = docs.iter::<One>().map(|r| r.unwrap().a).collect();
    assert_eq!(got, [1, 2]);
}

/// A line that is wholly a comment carries no value, exactly as a blank one
/// does. A trailing comment on a value line is the parser's, and it takes it.
#[test]
fn a_line_stream_skips_comment_only_lines() {
    let input = b"// header\n{\"a\":1} // one\n\n   /* whole line */   \n{\"a\":2}\n";
    let mut docs = Documents::lines(&input[..]).with_options::<AllowComments>();
    let got: Vec<_> = docs.iter::<One>().map(|r| r.unwrap().a).collect();
    assert_eq!(got, [1, 2]);
}

/// The one thing `Mode::Lines` cannot frame: a value's bytes are a line there,
/// so a block comment that spans lines is not one comment but two broken
/// lines. Each is reported and the framing survives, which is what makes the
/// value after them still arrive.
#[test]
fn a_line_stream_cannot_carry_a_comment_across_lines() {
    let input = b"/* opens here\n and closes here */\n{\"a\":1}\n";
    let mut docs = Documents::lines(&input[..]).with_options::<AllowComments>();
    let got: Vec<_> = docs.iter::<One>().collect();
    assert_eq!(got.len(), 3);
    assert!(got[0].is_err());
    assert!(got[1].is_err());
    assert_eq!(got[2].as_ref().unwrap().a, 1);
}

/// A comment a line does not finish is content, whatever it holds, including
/// nothing. `/*` alone is the case where the opener is the whole of the line,
/// and it has to be reported like any other unclosed one rather than passing
/// for a blank line.
#[test]
fn an_unclosed_comment_is_never_mistaken_for_a_blank_line() {
    for input in [
        &b"/*\n{\"a\":1}\n"[..],
        &b"/* x\n{\"a\":1}\n"[..],
        &b"/**//*\n{\"a\":1}\n"[..],
        &b"  /*  \n{\"a\":1}\n"[..],
    ] {
        let mut docs = Documents::lines(input).with_options::<AllowComments>();
        let got: Vec<_> = docs.iter::<One>().collect();
        assert_eq!(got.len(), 2, "{:?}", core::str::from_utf8(input));
        assert!(got[0].is_err(), "{:?}", core::str::from_utf8(input));
        assert_eq!(got[1].as_ref().unwrap().a, 1);
    }
}

/// Changing the policy discards the comment a scan was inside. There is no
/// coherent way to carry one across a change in whether comments are read, and
/// the alternative is a policy that reads none still honouring one.
#[test]
fn changing_the_policy_discards_an_open_comment() {
    let mut feed = structio::Feed::values().with_options::<AllowComments>();
    feed.push(b"/* not closed yet ");
    assert!(feed.next_value::<One>().is_none());

    let mut feed = feed.with_options::<Standard>();
    feed.push(b" */ {\"a\":1}");
    feed.end();
    assert!(feed.next_value::<One>().unwrap().is_err());
}

/// A comment's bytes are not part of any value, so the streaming readers step
/// over them without validating them as text. Inside a value's span the same
/// bytes are the parser's, and it takes the whole span as UTF-8 or not at all.
/// The batch entry points validate the document up front and so refuse both.
#[test]
fn a_streamed_comment_is_not_checked_as_text() {
    let mut docs = Documents::values(&b"/* \xFF */ {\"a\":1}"[..]).with_options::<AllowComments>();
    assert_eq!(docs.iter::<One>().next().unwrap().unwrap().a, 1);

    let mut docs = Documents::values(&b"{\"a\":1 /* \xFF */ }"[..]).with_options::<AllowComments>();
    assert_eq!(
        docs.iter::<One>()
            .next()
            .unwrap()
            .unwrap_err()
            .as_parse()
            .unwrap()
            .code,
        ErrorCode::InvalidUtf8
    );

    assert_eq!(
        structio::from_slice_with::<AllowComments, One>(b"/* \xFF */ {\"a\":1}")
            .unwrap_err()
            .code,
        ErrorCode::InvalidUtf8
    );
}

/// A stream that stops inside a block comment stopped inside a value's worth
/// of whitespace, and that is not a clean end.
#[test]
fn a_stream_that_ends_inside_a_comment_is_truncated() {
    let input = b"{\"a\":1} /* never closed";
    let mut docs = Documents::values(&input[..]).with_options::<AllowComments>();
    let mut it = docs.iter::<One>();
    assert_eq!(it.next().unwrap().unwrap().a, 1);
    assert!(it.next().unwrap().is_err());

    // A line comment is ended by the end of input, so this one is clean.
    let input = b"{\"a\":1} // done";
    let mut docs = Documents::values(&input[..]).with_options::<AllowComments>();
    let mut it = docs.iter::<One>();
    assert_eq!(it.next().unwrap().unwrap().a, 1);
    assert!(it.next().is_none());
}

/// A refill can land anywhere, including between the `*` and the `/` that
/// close a comment. Pushing one byte at a time is the worst case of that, in
/// the space between values and inside one.
#[test]
fn a_comment_survives_arriving_a_byte_at_a_time() {
    let input = b"/* lead */{\"a\": /* } */ 1}// tail\n/* gap */{\"a\":2}";

    let mut feed = structio::Feed::values().with_options::<AllowComments>();
    let mut got = Vec::new();
    for b in input {
        feed.push(&[*b]);
        while let Some(r) = feed.next_value::<One>() {
            got.push(r.unwrap().a);
        }
    }
    feed.end();
    while let Some(r) = feed.next_value::<One>() {
        got.push(r.unwrap().a);
    }
    assert_eq!(got, [1, 2]);
}

/// The splitter only frames and the parser decides, so the two have to agree
/// about which commented documents are documents at all. Chunked at one byte
/// as well as whole, because a refill may land anywhere in a comment.
#[test]
fn a_commented_stream_accepts_what_from_str_accepts() {
    let cases = [
        r#"{"a":1}"#,
        r#"/* lead */{"a":1}"#,
        r#"{"a":1}// tail"#,
        r#"{"a":1}/* tail */"#,
        r#"{"a": /* } */ 1}"#,
        r#"{"a":1 /* " */ }"#,
        r#"{/**/"a"/**/:/**/1/**/}"#,
        // And the malformed ones.
        r#"{"a":1 /* never closed"#,
        r#"/* never closed {"a":1}"#,
        r#"{"a":1}/x"#,
        r#"{"a":1}/"#,
        r#"{"a":1}/*"#,
    ];

    for text in cases {
        let batch = from_str_with::<AllowComments, One>(text).is_ok();
        for chunk in [1, 3, text.len()] {
            let mut feed = structio::Feed::values().with_options::<AllowComments>();
            let mut values = 0;
            let mut failed = false;
            for piece in text.as_bytes().chunks(chunk) {
                feed.push(piece);
                while let Some(r) = feed.next_value::<One>() {
                    match r {
                        Ok(_) => values += 1,
                        Err(_) => failed = true,
                    }
                }
                if failed {
                    break;
                }
            }
            if !failed {
                feed.end();
                while let Some(r) = feed.next_value::<One>() {
                    match r {
                        Ok(_) => values += 1,
                        Err(_) => failed = true,
                    }
                }
            }
            let streamed = !failed && values == 1;
            assert_eq!(batch, streamed, "disagreed about {text:?} in {chunk}s");
        }
    }
}
