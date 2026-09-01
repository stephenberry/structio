//! BEVE out as JSON, without a type in the middle.
//!
//! The property that carries most of the weight here is that the transcoder
//! agrees with the typed path: `beve_to_json(&to_beve(&v))` must be the same
//! bytes as `to_string(&v)`, for every shape the two formats both have. That
//! checks a new walk against two already-trusted ones, over every type, with
//! no golden text to keep up to date. The golden text below is for the cases
//! the typed path cannot produce: widths Rust has no type for, an extension,
//! and bytes no writer of this crate would emit.

use std::collections::BTreeMap;

use structio::beve::header;
use structio::{Complex, ErrorCode, Matrix, MatrixLayout, beve_to_json, to_beve, to_string};

#[derive(Default, Debug, PartialEq)]
struct Leaf {
    flag: bool,
    count: i64,
    ratio: f64,
    label: String,
}
structio::object!(Leaf {
    flag,
    count,
    ratio,
    label
});

#[derive(Default, Debug, PartialEq)]
struct Span {
    start: u32,
    end: u32,
}
structio::array!(Span [start, end]);

#[derive(Default, Debug, PartialEq)]
struct Doc {
    name: String,
    samples: Vec<f64>,
    flags: Vec<bool>,
    tags: Vec<String>,
    blob: Vec<u8>,
    narrow: f32,
    leaves: Vec<Leaf>,
    by_name: BTreeMap<String, i32>,
    by_index: BTreeMap<u16, i32>,
    maybe: Option<Leaf>,
    fixed: [i16; 3],
    span: Span,
}
structio::object!(Doc {
    name,
    samples,
    flags,
    tags,
    blob,
    narrow,
    leaves,
    by_name,
    by_index,
    maybe,
    fixed,
    span
});

fn doc() -> Doc {
    Doc {
        // Every character JSON has to escape, plus text that is not ASCII.
        name: "a\"b\\c\nd\te\u{1}f é 中 😀".into(),
        samples: vec![0.5, -1.25, 1e300, 0.0, -0.0],
        flags: vec![true, false, true, true, false, false, true, false, true],
        tags: vec!["alpha".into(), String::new(), "ω".into()],
        blob: vec![0, 1, 254, 255],
        narrow: 0.1,
        leaves: vec![
            Leaf {
                flag: true,
                count: i64::MIN,
                ratio: 2.5,
                label: "one".into(),
            },
            Leaf::default(),
        ],
        by_name: BTreeMap::from([("k".into(), -1), ("j".into(), 2)]),
        by_index: BTreeMap::from([(7u16, -20), (9, 40)]),
        maybe: Some(Leaf::default()),
        fixed: [i16::MIN, 0, i16::MAX],
        span: Span { start: 1, end: 2 },
    }
}

/// The whole `SIZE` encoding of `n`, for hand-built documents.
fn size(n: u64) -> Vec<u8> {
    let mut out = [0u8; 8];
    let used = header::encode_size(n, &mut out);
    out[..used].to_vec()
}

fn json_of(bytes: &[u8]) -> String {
    beve_to_json(bytes).expect("transcode")
}

fn code_of(bytes: &[u8]) -> ErrorCode {
    beve_to_json(bytes).expect_err("should not transcode").code
}

// ---------------------------------------------------------------------------
// Against the typed path
// ---------------------------------------------------------------------------

#[test]
fn the_output_matches_what_the_typed_writer_produces() {
    let value = doc();
    assert_eq!(json_of(&to_beve(&value)), to_string(&value));
}

#[test]
fn a_transcoded_document_parses_back_into_its_type() {
    let value = doc();
    let json = json_of(&to_beve(&value));
    assert_eq!(structio::from_str::<Doc>(&json).expect("parse"), value);
}

#[test]
fn every_scalar_matches() {
    macro_rules! same {
        ($($v:expr),* $(,)?) => {$(
            assert_eq!(json_of(&to_beve(&$v)), to_string(&$v), stringify!($v));
        )*}
    }
    same!(
        true,
        false,
        0u8,
        u8::MAX,
        i8::MIN,
        u16::MAX,
        i16::MIN,
        u32::MAX,
        i32::MIN,
        u64::MAX,
        i64::MIN,
        u128::MAX,
        i128::MIN,
        0.1f32,
        f32::MIN,
        0.1f64,
        f64::MAX,
        -0.0f64,
        "",
        "text",
    );
    // `Option::None` is the only way to reach a null through a declared type.
    let none: Option<u8> = None;
    assert_eq!(json_of(&to_beve(&none)), "null");
}

#[test]
fn empty_containers_keep_their_shape() {
    let empty = Doc::default();
    assert_eq!(json_of(&to_beve(&empty)), to_string(&empty));

    let no_numbers: Vec<f64> = Vec::new();
    assert_eq!(json_of(&to_beve(&no_numbers)), "[]");
    let no_strings: Vec<String> = Vec::new();
    assert_eq!(json_of(&to_beve(&no_strings)), "[]");
    let no_flags: Vec<bool> = Vec::new();
    assert_eq!(json_of(&to_beve(&no_flags)), "[]");
    let no_leaves: Vec<Leaf> = Vec::new();
    assert_eq!(json_of(&to_beve(&no_leaves)), "[]");
    let no_members: BTreeMap<String, i32> = BTreeMap::new();
    assert_eq!(json_of(&to_beve(&no_members)), "{}");
}

#[test]
fn non_finite_floats_become_null() {
    // No JSON form exists, so the writer emits null on both paths and the two
    // have to agree about it.
    let odd = vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0];
    assert_eq!(json_of(&to_beve(&odd)), "[null,null,null,1]");
    assert_eq!(json_of(&to_beve(&odd)), to_string(&odd));
}

#[test]
fn integer_keys_are_quoted() {
    let signed = BTreeMap::from([(-1i32, 10), (2, 20)]);
    assert_eq!(json_of(&to_beve(&signed)), r#"{"-1":10,"2":20}"#);
    assert_eq!(json_of(&to_beve(&signed)), to_string(&signed));

    let unsigned = BTreeMap::from([(1u64, 10), (u64::MAX, 20)]);
    assert_eq!(json_of(&to_beve(&unsigned)), to_string(&unsigned));
}

#[test]
fn a_narrow_float_keeps_the_digits_it_round_trips_through() {
    // `0.1f32` widened to an `f64` is 0.10000000149011612, and printing that
    // is both wrong-looking and not what the typed writer does. Going out
    // through the `f32` path is what keeps it "0.1".
    assert_eq!(json_of(&to_beve(&0.1f32)), "0.1");
    assert_eq!(json_of(&to_beve(&vec![0.1f32, 0.2])), "[0.1,0.2]");
}

// ---------------------------------------------------------------------------
// Shapes no declared type produces
// ---------------------------------------------------------------------------

#[test]
fn the_two_sixteen_bit_floats_widen_losslessly() {
    // Codes 0 and 1 under the float category are bfloat16 and float16; there
    // is no 8-bit float for them to have been.
    let bf16 = [
        &[header::number(header::CAT_FLOAT, 0)][..],
        &0x3FC0u16.to_le_bytes(),
    ]
    .concat();
    assert_eq!(json_of(&bf16), "1.5");

    let f16 = [
        &[header::number(header::CAT_FLOAT, 1)][..],
        &0x3E00u16.to_le_bytes(),
    ]
    .concat();
    assert_eq!(json_of(&f16), "1.5");

    // A typed array of them takes the same path, one element at a time.
    let run = [
        &[header::array_of(header::CAT_FLOAT, 1)][..],
        &size(2),
        &0x3E00u16.to_le_bytes(),
        &0xC000u16.to_le_bytes(),
    ]
    .concat();
    assert_eq!(json_of(&run), "[1.5,-2]");
}

#[test]
fn the_aligned_form_reads_like_the_ordinary_one() {
    // `HEADER | NUMERIC_HEADER | SIZE | PADDING_LENGTH | PADDING | DATA`. The
    // writer never emits this, but other implementations do.
    let aligned = [
        &[
            header::ALIGNED_ARRAY,
            header::array_of(header::CAT_UNSIGNED, 2),
        ][..],
        &size(2),
        &[3, 0, 0, 0],
        &7u32.to_le_bytes(),
        &8u32.to_le_bytes(),
    ]
    .concat();
    assert_eq!(json_of(&aligned), "[7,8]");
    assert_eq!(json_of(&aligned), json_of(&to_beve(&vec![7u32, 8])));
}

#[test]
fn an_object_with_wide_integer_keys_transcodes() {
    // A `u64`-keyed object, which no `object!` struct can be but another
    // implementation may write.
    let object = [
        &[header::header(header::TY_OBJECT, header::CAT_UNSIGNED, 3)][..],
        &size(1),
        &u64::MAX.to_le_bytes(),
        &[header::TRUE],
    ]
    .concat();
    assert_eq!(json_of(&object), r#"{"18446744073709551615":true}"#);
}

// ---------------------------------------------------------------------------
// What is refused
// ---------------------------------------------------------------------------

#[test]
fn the_extensions_that_carry_nothing_are_refused() {
    // Both state their own extent, so a reader steps over either. Neither is a
    // value: a delimiter separates documents and the type tag is deprecated.
    for ext in [header::EXT_DELIMITER, header::EXT_TYPE_TAG] {
        let bytes = [(ext << 3) | header::TY_EXTENSION];
        assert_eq!(code_of(&bytes), ErrorCode::UnsupportedFeature, "{ext}");
    }
}

#[test]
fn the_two_extensions_that_carry_data_transcode_to_what_the_types_write() {
    // Not an encoding chosen here: these are the forms `Complex` and `Matrix`
    // write in JSON and read back from BEVE, so a transcode changes the format
    // and not the meaning. Both directions are asserted against each other so
    // one cannot drift.
    let z = Complex::new(1.5f64, -2.5);
    let run = vec![Complex::new(1.0f32, 2.0), Complex::new(3.0, 4.0)];
    let m = Matrix::new(MatrixLayout::ColumnMajor, vec![1, 3], vec![7u16, 8, 9]).unwrap();
    let zm = Matrix::new(
        MatrixLayout::RowMajor,
        vec![2],
        vec![Complex::new(0.5f64, 1.5), Complex::new(2.5, 3.5)],
    )
    .unwrap();

    assert_eq!(json_of(&to_beve(&z)), "[1.5,-2.5]");
    assert_eq!(json_of(&to_beve(&run)), "[[1,2],[3,4]]");
    assert_eq!(
        json_of(&to_beve(&m)),
        r#"{"layout":"layout_left","extents":[1,3],"value":[7,8,9]}"#
    );
    assert_eq!(json_of(&to_beve(&zm)), structio::to_string(&zm));

    // An empty run of complex numbers is still an array, not a lone pair.
    let none: Vec<Complex<f64>> = Vec::new();
    assert_eq!(json_of(&to_beve(&none)), "[]");
}

#[test]
fn a_matrix_layout_that_is_not_defined_is_refused_rather_than_guessed() {
    // The byte is one byte wherever it points, so `validate` has no reason to
    // look at it and does not. Writing it out is a different matter: naming one
    // of the two defined layouts here would transpose the data silently.
    let mut m = to_beve(&Matrix::new(MatrixLayout::RowMajor, vec![2], vec![1u8, 2]).unwrap());
    m[1] = 9;
    assert!(structio::validate_beve(&m).is_ok());
    assert_eq!(code_of(&m), ErrorCode::InvalidMatrixLayout);
}

#[test]
fn a_128_bit_float_is_refused() {
    let bytes = [&[header::number(header::CAT_FLOAT, 4)][..], &[0u8; 16]].concat();
    assert_eq!(code_of(&bytes), ErrorCode::UnsupportedFeature);
    // The refusal has to survive being an array element too, where the header
    // is read once for the whole run.
    let run = [
        &[header::array_of(header::CAT_FLOAT, 4)][..],
        &size(1),
        &[0u8; 16],
    ]
    .concat();
    assert_eq!(code_of(&run), ErrorCode::UnsupportedFeature);
}

#[test]
fn a_string_that_is_not_utf8_is_refused() {
    // The JSON writer's buffer is handed out as a `String` without
    // revalidation, so this is the check that keeps it honest.
    let bad = [&[header::STRING][..], &size(2), &[0xFF, 0xFE]].concat();
    assert_eq!(code_of(&bad), ErrorCode::InvalidUtf8);

    let key = [
        &[header::OBJECT][..],
        &size(1),
        &size(1),
        &[0xFF],
        &[header::NULL],
    ]
    .concat();
    assert_eq!(code_of(&key), ErrorCode::InvalidUtf8);

    let element = [&[header::STRING_ARRAY][..], &size(1), &size(1), &[0x80]].concat();
    assert_eq!(code_of(&element), ErrorCode::InvalidUtf8);
}

#[test]
fn a_document_must_be_whole_and_alone() {
    let bytes = to_beve(&doc());
    for cut in 0..bytes.len() {
        assert!(beve_to_json(&bytes[..cut]).is_err(), "truncated at {cut}");
    }
    let mut trailing = bytes.clone();
    trailing.push(header::NULL);
    assert_eq!(code_of(&trailing), ErrorCode::TrailingContent);
}

#[test]
fn a_count_larger_than_the_input_is_refused_rather_than_walked() {
    // A count comes off the wire and need not describe the bytes behind it.
    let lying = [
        &[header::array_of(header::CAT_UNSIGNED, 3)][..],
        &size(1 << 40),
    ]
    .concat();
    assert_eq!(code_of(&lying), ErrorCode::UnexpectedEnd);

    let bools = [&[header::BOOL_ARRAY][..], &size(1 << 40)].concat();
    assert_eq!(code_of(&bools), ErrorCode::UnexpectedEnd);

    let strings = [&[header::STRING_ARRAY][..], &size(1 << 40)].concat();
    assert_eq!(code_of(&strings), ErrorCode::UnexpectedEnd);
}

/// A stack of `depth` containers around a single null, alternating the two
/// kinds so that both of the levels charged, and both of the ones given back,
/// are exercised.
fn nested(depth: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    for level in 0..depth {
        if level % 2 == 0 {
            bytes.push(header::GENERIC_ARRAY);
            bytes.extend_from_slice(&size(1));
        } else {
            // One member, under the empty key.
            bytes.push(header::OBJECT);
            bytes.extend_from_slice(&size(1));
            bytes.extend_from_slice(&size(0));
        }
    }
    bytes.push(header::NULL);
    bytes
}

#[test]
fn nesting_is_accepted_exactly_where_the_reader_accepts_it() {
    // Both walk the same containers and charge the same levels, so a document
    // one of them takes and the other refuses would be a bug in whichever is
    // the odd one out.
    for depth in 250..260u32 {
        let bytes = nested(depth);
        assert_eq!(
            beve_to_json(&bytes).is_ok(),
            structio::validate_beve(&bytes).is_ok(),
            "depth {depth}"
        );
    }
    assert_eq!(code_of(&nested(1000)), ErrorCode::ExceededMaxDepth);
}

#[test]
fn the_json_is_shallow_enough_to_read_back() {
    // A typed array costs no recursion but is a bracket level all the same, so
    // a walk that let it through free would emit, at the nesting limit, JSON
    // one level deeper than this crate's own parser accepts. The two limits are
    // the same number, which leaves no room for the output to overshoot by one.
    // `outer` generic arrays around one typed array, which is the shape that
    // puts a typed array at the very bottom of a document.
    let doc = |outer: u32| -> Vec<u8> {
        let mut bytes = Vec::new();
        for _ in 0..outer {
            bytes.push(header::GENERIC_ARRAY);
            bytes.extend_from_slice(&size(1));
        }
        bytes.push(header::array_of(header::CAT_UNSIGNED, 0));
        bytes.extend_from_slice(&size(1));
        bytes.push(7);
        bytes
    };

    // The deepest one that transcodes at all, found rather than assumed: a
    // constant here would be a number chosen to agree with the walk, which is
    // the thing under test.
    let deepest = (0..2 * structio::beve::reader::MAX_DEPTH)
        .map(doc)
        .take_while(|bytes| beve_to_json(bytes).is_ok())
        .last()
        .expect("some depth transcodes");

    let json = json_of(&deepest);
    let mut parser = structio::json::Parser::new(&json);
    parser
        .skip_value()
        .and_then(|()| parser.finish())
        .unwrap_or_else(|e| {
            panic!(
                "the deepest transcodable document produced {} bracket levels, \
                 which this crate's own parser refuses: {}",
                json.bytes().filter(|&b| b == b'[').count(),
                e.message()
            )
        });
}

#[test]
fn depth_given_back_is_depth_charged() {
    // Nesting is a running total, so a container that takes a level without
    // returning it makes a wide document look like a deep one. A hundred
    // containers in a row, one after another rather than one inside another,
    // must not add up to anything.
    let deepest = nested(structio::beve::reader::MAX_DEPTH - 1);
    let wide = [
        &[header::GENERIC_ARRAY][..],
        &size(4),
        &deepest,
        &deepest,
        &deepest,
        &deepest,
    ]
    .concat();
    assert!(beve_to_json(&wide).is_ok(), "a wide document was refused");
    assert!(structio::validate_beve(&wide).is_ok());
}

#[test]
fn an_error_carries_the_offset_it_happened_at() {
    let bytes = [&[header::GENERIC_ARRAY][..], &size(2), &[header::NULL]].concat();
    let err = beve_to_json(&bytes).expect_err("the second element is missing");
    assert_eq!(err.code, ErrorCode::UnexpectedEnd);
    assert_eq!(err.index, bytes.len());
}

// ---------------------------------------------------------------------------
// The other two entry points
// ---------------------------------------------------------------------------

#[test]
fn the_sink_output_matches_the_in_memory_string() {
    let bytes = to_beve(&doc());
    let want = json_of(&bytes);
    let mut got = Vec::new();
    structio::beve_to_json_writer(&bytes, &mut got).expect("transcode");
    assert_eq!(String::from_utf8(got).expect("utf-8"), want);

    // At every buffer size, including ones that cut through a value and ones
    // that leave a trailing comma as the only byte held back for the closing
    // brace to overwrite.
    for cap in 1..=want.len() + 4 {
        let mut got = Vec::new();
        structio::beve_to_json_writer_buffered(&bytes, &mut got, cap).expect("transcode");
        assert_eq!(
            String::from_utf8(got).expect("utf-8"),
            want,
            "buffer of {cap}"
        );
    }
}

#[test]
fn a_failing_sink_reports_its_error() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("no"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let bytes = to_beve(&doc());
    let err = structio::beve_to_json_writer(&bytes, Broken).expect_err("the sink refused");
    assert!(err.as_io().is_some(), "{err}");
}

#[test]
fn a_malformed_document_reports_content_rather_than_io() {
    let mut got = Vec::new();
    let err = structio::beve_to_json_writer(&[header::GENERIC_ARRAY], &mut got)
        .expect_err("the count is missing");
    assert_eq!(
        err.as_parse().expect("a parse failure").code,
        ErrorCode::UnexpectedEnd
    );
}

#[test]
fn writing_into_an_existing_string_keeps_its_allocation() {
    let bytes = to_beve(&doc());
    let mut out = String::new();
    structio::beve_to_json_into(&bytes, &mut out).expect("transcode");
    assert_eq!(out, json_of(&bytes));

    let at = out.as_ptr();
    structio::beve_to_json_into(&bytes, &mut out).expect("transcode");
    assert_eq!(out.as_ptr(), at, "the allocation was reused");
    assert_eq!(out, json_of(&bytes));
}

#[test]
fn a_failed_transcode_leaves_the_string_valid() {
    let mut out = String::from("stale");
    let err = structio::beve_to_json_into(&[header::GENERIC_ARRAY, 0x08], &mut out)
        .expect_err("the element is missing");
    assert_eq!(err.code, ErrorCode::UnexpectedEnd);
    // Whatever was written before the failure, and nothing of what was there
    // before the call.
    assert_eq!(out, "[");
}
