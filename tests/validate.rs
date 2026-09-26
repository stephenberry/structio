//! Checking a BEVE document without decoding it.
//!
//! Validation walks the same headers reading does, so what it must never do is
//! disagree with reading about where a value ends: a document that validates
//! and then fails to read for a structural reason, or the reverse, would mean
//! two different notions of the format. Most of what is here is that
//! agreement, checked over every construct the writer can emit and over
//! corruption of each one.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};

use structio::beve::header;
use structio::{
    Complex, ErrorCode, Matrix, MatrixLayout, Number, SkipUnknown, Value, beve, beve_to_json,
    from_beve, from_beve_at, from_beve_with, read_beve_array_into, to_beve, to_beve_aligned,
    validate_beve,
};

#[derive(Default, Debug, PartialEq)]
struct Everything {
    id: i64,
    name: String,
    tags: Vec<String>,
    values: Vec<f64>,
    flags: Vec<bool>,
    blob: Vec<u8>,
    maybe: Option<u32>,
    inner: Inner,
    lookup: HashMap<u32, String>,
}
structio::object!(Everything {
    id,
    name,
    tags,
    values,
    flags,
    blob,
    maybe,
    inner,
    lookup
});

#[derive(Default, Debug, PartialEq)]
struct Inner {
    depth: u8,
    ratio: f32,
}
structio::object!(Inner { depth, ratio });

fn everything() -> Everything {
    Everything {
        id: -42,
        name: "sensor/1".into(),
        tags: vec!["a".into(), "".into(), "ünïcøde".into()],
        values: vec![1.5, f64::NAN, f64::NEG_INFINITY],
        flags: vec![true, false, true, true, false, false, true, false, true],
        blob: vec![0, 1, 255],
        maybe: Some(7),
        inner: Inner {
            depth: 3,
            ratio: 0.25,
        },
        lookup: HashMap::from([(1, "one".to_string()), (2, "two".to_string())]),
    }
}

// ---------------------------------------------------------------------------
// Agreement with reading
// ---------------------------------------------------------------------------

#[test]
fn a_document_the_writer_produced_validates() {
    validate_beve(&to_beve(&everything())).unwrap();
}

#[test]
fn every_scalar_and_container_validates() {
    validate_beve(&to_beve(&())).unwrap();
    validate_beve(&to_beve(&true)).unwrap();
    validate_beve(&to_beve(&0u8)).unwrap();
    validate_beve(&to_beve(&i128::MIN)).unwrap();
    validate_beve(&to_beve(&f32::NAN)).unwrap();
    validate_beve(&to_beve("")).unwrap();
    validate_beve(&to_beve(&Vec::<f64>::new())).unwrap();
    validate_beve(&to_beve(&vec![vec![1u16], vec![]])).unwrap();
    validate_beve(&to_beve(&(1u8, "two", 3.0f64))).unwrap();
    validate_beve(&to_beve(&HashMap::from([("k", 1u8)]))).unwrap();
}

#[test]
fn truncation_at_any_point_is_rejected_by_both_walks() {
    let bytes = to_beve(&everything());
    for n in 0..bytes.len() {
        let head = &bytes[..n];
        assert!(
            validate_beve(head).is_err(),
            "a truncated document validated at {n} bytes"
        );
        // The two walks must agree about which prefixes are documents, or a
        // validator would be worth nothing as a gate in front of a reader.
        assert!(from_beve::<Everything>(head).is_err(), "read back at {n}");
    }
}

/// Absorbs any nesting depth, so reading can be compared against the other two
/// walks at the limit rather than only at a depth a concrete type can spell.
#[derive(Default, Debug)]
struct Chain {
    next: Option<Box<Chain>>,
}
structio::object!(Chain { next });

#[test]
fn the_three_walks_agree_at_the_nesting_limit() {
    // The limit counts containers, not values. Charging a scalar a level too
    // would put validation one tighter than reading, so it would reject a
    // document the reader accepts. Charging one too few is the worse of the
    // two and has its own test below, since that is a gate passing input the
    // parser then refuses. Skipping is the same walk and has to land in the
    // same place as both.
    fn chain(depth: usize) -> Vec<u8> {
        let mut doc = Vec::new();
        for _ in 0..depth {
            doc.extend_from_slice(&[header::OBJECT, 1 << 2, 4 << 2]);
            doc.extend_from_slice(b"next");
        }
        doc.push(header::NULL);
        doc
    }

    for (depth, ok) in [(1, true), (255, true), (256, true), (257, false)] {
        let doc = chain(depth);
        assert_eq!(from_beve::<Chain>(&doc).is_ok(), ok, "read at {depth}");
        assert_eq!(validate_beve(&doc).is_ok(), ok, "validate at {depth}");
        assert_eq!(
            beve::Reader::new(&doc).skip_value().is_ok(),
            ok,
            "skip at {depth}"
        );
    }

    assert_eq!(
        validate_beve(&chain(257)).unwrap_err().code,
        ErrorCode::ExceededMaxDepth
    );
}

#[test]
fn corrupting_any_single_byte_is_caught_or_read_back_but_never_both_ways_round() {
    let bytes = to_beve(&everything());
    for i in 0..bytes.len() {
        for bit in 0..8 {
            let mut corrupt = bytes.clone();
            corrupt[i] ^= 1 << bit;
            if corrupt == bytes {
                continue;
            }
            // A flipped bit may leave a document that is still well formed and
            // merely says something else. What it must never do is validate
            // and then fail to read for a *structural* reason: that would mean
            // the two disagreed about the shape of the same bytes.
            if validate_beve(&corrupt).is_ok()
                && let Err(e) = from_beve::<Everything>(&corrupt)
            {
                assert!(
                    !matches!(
                        e.code,
                        ErrorCode::UnexpectedEnd
                            | ErrorCode::TrailingContent
                            | ErrorCode::InvalidHeader
                            | ErrorCode::InvalidPadding
                            | ErrorCode::ExceededMaxDepth
                            | ErrorCode::InvalidUtf8
                    ),
                    "byte {i} bit {bit} validated but read back as {:?}",
                    e.code
                );
            }
            // And the other way round, which is the direction that actually
            // matters for a gate: nothing the reader accepts may fail to
            // validate. UTF-8 is the one exception, since a corrupted string
            // in a field no struct claims is skipped unread.
            if from_beve::<Everything>(&corrupt).is_ok()
                && let Err(e) = validate_beve(&corrupt)
            {
                assert_eq!(
                    e.code,
                    ErrorCode::InvalidUtf8,
                    "byte {i} bit {bit} read back but failed to validate"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// What validation refuses
// ---------------------------------------------------------------------------

#[test]
fn a_document_must_hold_exactly_one_value() {
    assert_eq!(
        validate_beve(&[]).unwrap_err().code,
        ErrorCode::UnexpectedEnd
    );

    let mut two = to_beve(&1u8);
    two.extend_from_slice(&to_beve(&2u8));
    assert_eq!(
        validate_beve(&two).unwrap_err().code,
        ErrorCode::TrailingContent
    );
}

#[test]
fn a_delimiter_separated_stream_is_several_documents_and_not_one() {
    let mut stream = to_beve(&1u8);
    stream.push(header::header(header::TY_EXTENSION, 0, 0));
    stream.extend_from_slice(&to_beve(&2u8));
    assert_eq!(
        validate_beve(&stream).unwrap_err().code,
        ErrorCode::TrailingContent
    );
}

#[test]
fn a_delimiter_is_not_a_value_anywhere_one_belongs() {
    // It separates documents and is never one, so every walk that expects a
    // value refuses it there, and with the same code. A walk that stepped over
    // it as a value of no extent would pass a document that frames as nothing
    // and that no reader can read.
    let d = header::DELIMITER;
    let cases: [(&str, &[u8]); 4] = [
        ("alone", &[d]),
        ("before a value", &[d, header::TRUE]),
        (
            "as a member's value",
            &[header::OBJECT, 1 << 2, 1 << 2, b'z', d],
        ),
        ("as an array element", &[header::GENERIC_ARRAY, 1 << 2, d]),
    ];
    for (name, bytes) in cases {
        let code = |r: Result<(), structio::Error>| r.unwrap_err().code;
        assert_eq!(
            code(validate_beve(bytes)),
            ErrorCode::InvalidHeader,
            "validate {name}"
        );
        assert_eq!(
            code(from_beve::<structio::Value>(bytes).map(drop)),
            ErrorCode::InvalidHeader,
            "Value {name}"
        );
        assert_eq!(
            code(structio::beve_to_json(bytes).map(drop)),
            ErrorCode::InvalidHeader,
            "transcode {name}"
        );
    }

    // Inside the two extensions that wrap values, which the readers refuse or
    // read as a type before ever reaching an operand, the validator is the walk
    // that meets it.
    let tag = (header::EXT_TYPE_TAG << 3) | header::TY_EXTENSION;
    for bytes in [vec![tag, 0, d], vec![header::MATRIX, 0, d, header::NULL]] {
        assert_eq!(
            validate_beve(&bytes).unwrap_err().code,
            ErrorCode::InvalidHeader,
            "{bytes:02x?}"
        );
    }

    // A pointer aimed into one, or past one, finds the document at fault
    // rather than the pointer.
    let seek = |bytes: &[u8], pointer| {
        beve::from_slice_at::<structio::Value>(bytes, pointer)
            .unwrap_err()
            .code
    };
    assert_eq!(seek(&[d], "/z"), ErrorCode::InvalidHeader);
    let past = [header::GENERIC_ARRAY, 2 << 2, d, header::NULL];
    assert_eq!(seek(&past, "/1"), ErrorCode::InvalidHeader);
}

#[test]
fn a_string_that_is_not_utf8_is_rejected() {
    // The writer cannot produce one, so build it: header, size, payload.
    let bad = [header::STRING, 2 << 2, 0xff, 0xfe];
    assert_eq!(
        validate_beve(&bad).unwrap_err().code,
        ErrorCode::InvalidUtf8
    );
}

#[test]
fn an_object_key_that_is_not_utf8_is_rejected() {
    let bad = [header::OBJECT, 1 << 2, 1 << 2, 0xff, header::NULL];
    assert_eq!(
        validate_beve(&bad).unwrap_err().code,
        ErrorCode::InvalidUtf8
    );
}

#[test]
fn a_string_array_element_that_is_not_utf8_is_rejected() {
    let bad = [header::STRING_ARRAY, 1 << 2, 1 << 2, 0x80];
    assert_eq!(
        validate_beve(&bad).unwrap_err().code,
        ErrorCode::InvalidUtf8
    );
}

#[test]
fn skipping_still_does_not_look_at_string_bytes() {
    // The same bad string, in a field no struct claims. Skipping is not
    // validation and must stay as cheap as it was.
    let mut doc = vec![header::OBJECT, 2 << 2];
    doc.extend_from_slice(&[1 << 2, b'z']); // key "z"
    doc.extend_from_slice(&[header::STRING, 2 << 2, 0xff, 0xfe]);
    doc.extend_from_slice(&[5 << 2, b'd', b'e', b'p', b't', b'h']); // key "depth"
    doc.extend_from_slice(&[header::number(header::CAT_UNSIGNED, 0), 4]);

    assert_eq!(from_beve_with::<SkipUnknown, Inner>(&doc).unwrap().depth, 4);
    assert_eq!(
        validate_beve(&doc).unwrap_err().code,
        ErrorCode::InvalidUtf8
    );
}

#[test]
fn the_three_extensions_that_are_values_validate() {
    let ext = |id: u8| header::header(header::TY_EXTENSION, 0, 0) | (id << 3);
    let bytes = |v: [u8; 2]| {
        vec![
            header::array_of(header::CAT_UNSIGNED, 0),
            2 << 2,
            v[0],
            v[1],
        ]
    };

    // The deprecated type tag: an index, then the value it tagged. Validation
    // recurses through it, so a bad string inside one is still caught.
    let mut tag = vec![ext(header::EXT_TYPE_TAG), 3 << 2];
    tag.extend_from_slice(&[header::STRING, 1 << 2, b'x']);
    validate_beve(&tag).unwrap();
    let mut bad_tag = vec![ext(header::EXT_TYPE_TAG), 3 << 2];
    bad_tag.extend_from_slice(&[header::STRING, 1 << 2, 0xff]);
    assert_eq!(
        validate_beve(&bad_tag).unwrap_err().code,
        ErrorCode::InvalidUtf8
    );

    // A layout byte, then the extents and the data, both typed arrays.
    let mut matrix = vec![ext(header::EXT_MATRIX), 0];
    matrix.extend_from_slice(&bytes([2, 1]));
    matrix.extend_from_slice(&bytes([7, 8]));
    validate_beve(&matrix).unwrap();

    // One complex f64: the flag byte says a single pair, then two payloads.
    let mut complex = vec![ext(header::EXT_COMPLEX)];
    complex.push(header::number(header::CAT_FLOAT, 3) & !0b111);
    complex.extend_from_slice(&[0u8; 16]);
    validate_beve(&complex).unwrap();
}

#[test]
fn an_undefined_null_or_boolean_header_is_not_a_value() {
    // Only three of the four sub-codes are defined, and the byte-count field
    // must be zero. Guessing at the rest is what this crate refuses to do.
    for h in [0b0001_0000u8, 0b1110_0000, 0b0010_0000] {
        assert_eq!(
            validate_beve(&[h]).unwrap_err().code,
            ErrorCode::InvalidHeader,
            "{h:#010b}"
        );
        // A reader that wanted a boolean finds a header of the right type that
        // is no value at all, not a value of the wrong kind.
        for e in [
            from_beve::<bool>(&[h]).unwrap_err(),
            from_beve::<Option<bool>>(&[h]).unwrap_err(),
            from_beve::<Value>(&[h]).unwrap_err(),
            beve_to_json(&[h]).unwrap_err(),
        ] {
            assert_eq!(
                (e.code, e.index),
                (ErrorCode::InvalidHeader, 1),
                "{h:#010b}"
            );
        }
    }
    for h in [header::NULL, header::FALSE, header::TRUE] {
        validate_beve(&[h]).unwrap();
    }
}

#[test]
fn an_undefined_float_width_is_not_a_value_in_any_walk() {
    // A float has five widths, codes 0 to 4. Codes 5 to 7 describe no number,
    // so a reader that wanted an integer has not met a float it can refuse by
    // kind: it has met a header with no extent, as every other walk has. The
    // refusal is on the header, so the offset is just past it.
    let integers: [(&str, Walk); 12] = [
        ("u8", |b| from_beve::<u8>(b).map(drop)),
        ("u16", |b| from_beve::<u16>(b).map(drop)),
        ("u32", |b| from_beve::<u32>(b).map(drop)),
        ("u64", |b| from_beve::<u64>(b).map(drop)),
        ("u128", |b| from_beve::<u128>(b).map(drop)),
        ("i8", |b| from_beve::<i8>(b).map(drop)),
        ("i16", |b| from_beve::<i16>(b).map(drop)),
        ("i32", |b| from_beve::<i32>(b).map(drop)),
        ("i64", |b| from_beve::<i64>(b).map(drop)),
        ("i128", |b| from_beve::<i128>(b).map(drop)),
        ("Option<u64>", |b| from_beve::<Option<u64>>(b).map(drop)),
        ("pointer u64", |b| {
            beve::from_slice_at::<u64>(b, "").map(drop)
        }),
    ];
    // Every walk that decodes a float, a `Number` and a `Value` included.
    let floats: [(&str, Walk); 7] = [
        ("f32", |b| from_beve::<f32>(b).map(drop)),
        ("f64", |b| from_beve::<f64>(b).map(drop)),
        ("Option<f64>", |b| from_beve::<Option<f64>>(b).map(drop)),
        ("Number", |b| from_beve::<Number>(b).map(drop)),
        ("Value", |b| from_beve::<Value>(b).map(drop)),
        ("pointer Value", |b| {
            beve::from_slice_at::<Value>(b, "").map(drop)
        }),
        ("transcode", |b| beve_to_json(b).map(drop)),
    ];
    // The walks that only measure a value, and so have no use for its type.
    let measuring: [(&str, Walk); 2] = [("validate", validate_beve), ("skip", skipped)];
    for code in 5..8 {
        let mut doc = vec![header::number(header::CAT_FLOAT, code)];
        doc.extend_from_slice(&[0; 16]);
        for (name, walk) in integers.iter().chain(&floats).chain(&measuring) {
            let e = walk(&doc).unwrap_err();
            assert_eq!(
                (e.code, e.index),
                (ErrorCode::InvalidHeader, 1),
                "{name}, code {code}"
            );
        }
        // The cursor itself, which a caller trying another reading starts from.
        let mut r = beve::Reader::new(&doc);
        assert_eq!(r.read_i64(), Err(ErrorCode::InvalidHeader), "code {code}");
        assert_eq!(r.position(), 1, "code {code}");
        // The framer reports against the value's start rather than past its
        // header, as it does for every refusal, so only its code is compared.
        let mut docs = beve::Documents::values(&doc[..]);
        let e = docs.next_value::<u64>().unwrap().unwrap_err();
        assert_eq!(e.as_parse().unwrap().code, ErrorCode::InvalidHeader);
    }

    // Code 4 is a width the format defines, a 128-bit float, which nothing here
    // has a type for. Every walk that decodes it refuses it on the header, by
    // kind for an integer and as unsupported otherwise, so whether its payload
    // is there makes no difference. The walks that measure it step over it,
    // and so need the payload.
    let mut f128 = vec![header::number(header::CAT_FLOAT, 4)];
    f128.extend_from_slice(&[0; 16]);
    for doc in [&f128[..], &f128[..1]] {
        let whole = doc.len() > 1;
        for (name, walk) in integers {
            let e = walk(doc).unwrap_err();
            assert_eq!(
                (e.code, e.index),
                (ErrorCode::ExpectedInteger, 1),
                "{name}, whole {whole}"
            );
        }
        for (name, walk) in floats {
            let e = walk(doc).unwrap_err();
            assert_eq!(
                (e.code, e.index),
                (ErrorCode::UnsupportedFeature, 1),
                "{name}, whole {whole}"
            );
        }
        for (name, walk) in measuring {
            match walk(doc) {
                Ok(()) => assert!(whole, "{name}"),
                Err(e) => assert_eq!(
                    (e.code, e.index, whole),
                    (ErrorCode::UnexpectedEnd, 1, false),
                    "{name}"
                ),
            }
        }
    }
}

#[test]
fn a_128_bit_float_in_a_container_is_refused_where_its_first_element_begins() {
    // A typed array or a complex value states its element type once, up front,
    // and every walk that decodes the elements refuses one it cannot decode
    // there, before the payload is looked for. An empty one holds nothing to
    // decode.
    let f128 = header::number(header::CAT_FLOAT, 4);
    let class = f128 & !0b111;
    let cases: [(&str, Vec<u8>, Option<usize>); 6] = [
        (
            "typed array",
            [
                &[header::array_of(header::CAT_FLOAT, 4), 1 << 2][..],
                &[0; 16],
            ]
            .concat(),
            Some(2),
        ),
        (
            "empty typed array",
            vec![header::array_of(header::CAT_FLOAT, 4), 0],
            None,
        ),
        (
            "complex",
            [&[header::COMPLEX, class][..], &[0; 32]].concat(),
            Some(2),
        ),
        (
            "complex run",
            [&[header::COMPLEX, class | 1, 1 << 2][..], &[0; 32]].concat(),
            Some(3),
        ),
        (
            "empty complex run",
            vec![header::COMPLEX, class | 1, 0],
            None,
        ),
        (
            "matrix",
            [
                &[
                    header::MATRIX,
                    0,
                    header::array_of(header::CAT_UNSIGNED, 0),
                    1 << 2,
                    1,
                ][..],
                &[header::array_of(header::CAT_FLOAT, 4), 1 << 2],
                &[0; 16],
            ]
            .concat(),
            Some(7),
        ),
    ];
    // Each refused one is tried whole and with its payload cut short. The
    // refusal is on the header, so the missing payload makes no difference to
    // a walk that decodes, where a validator, which steps over the payload and
    // accepts it whole, runs out at the same place.
    let short: Vec<_> = cases
        .iter()
        .filter_map(|&(name, ref doc, at)| at.map(|at| (name, doc[..at + 8].to_vec(), Some(at))))
        .collect();
    let whole = cases.into_iter().map(|case| (case, true));
    for ((name, doc, refused_at), whole) in whole.chain(short.into_iter().map(|case| (case, false)))
    {
        let validated = validate_beve(&doc).map_err(|e| (e.code, e.index));
        match refused_at {
            Some(at) if !whole => {
                assert_eq!(validated, Err((ErrorCode::UnexpectedEnd, at)), "{name}")
            }
            _ => assert_eq!(validated, Ok(()), "{name}"),
        }
        let typed: structio::Result<()> = if doc[0] == header::COMPLEX {
            if doc[1] == class {
                from_beve::<Complex<f64>>(&doc).map(drop)
            } else {
                from_beve::<Vec<Complex<f64>>>(&doc).map(drop)
            }
        } else if doc[0] == header::MATRIX {
            from_beve::<structio::Matrix<f64>>(&doc).map(drop)
        } else {
            from_beve::<Vec<f64>>(&doc).map(drop)
        };
        for (walk, r) in [
            ("typed", typed),
            ("Value", from_beve::<Value>(&doc).map(drop)),
            (
                "pointer Value",
                beve::from_slice_at::<Value>(&doc, "").map(drop),
            ),
            ("transcode", beve_to_json(&doc).map(drop)),
        ] {
            match refused_at {
                None => r.unwrap_or_else(|e| panic!("{walk}, {name}: {e:?}")),
                Some(at) => {
                    let e = r.unwrap_err();
                    assert_eq!(
                        (e.code, e.index),
                        (ErrorCode::UnsupportedFeature, at),
                        "{walk}, {name}, whole {whole}"
                    );
                }
            }
        }
    }
}

type Walk = fn(&[u8]) -> structio::Result<()>;

#[derive(Default, Debug)]
enum Named {
    #[default]
    A,
}
structio::tagged_enum!(Named { A });

#[derive(Default, Debug)]
enum Tagged {
    #[default]
    A,
}
structio::tagged_enum!(Tagged as tag "kind" { A });

/// The typed reads that want the kind of value `h` heads, whatever else about
/// it they might go on to refuse.
fn readers_of(h: u8) -> Vec<(&'static str, Walk)> {
    match header::ty(h) {
        header::TY_NULL_BOOL => vec![
            ("bool", |b| from_beve::<bool>(b).map(drop)),
            ("Option<bool>", |b| from_beve::<Option<bool>>(b).map(drop)),
            ("()", |b| from_beve::<()>(b).map(drop)),
        ],
        header::TY_NUMBER => vec![
            ("u8", |b| from_beve::<u8>(b).map(drop)),
            ("i32", |b| from_beve::<i32>(b).map(drop)),
            ("u64", |b| from_beve::<u64>(b).map(drop)),
            ("i128", |b| from_beve::<i128>(b).map(drop)),
            ("f32", |b| from_beve::<f32>(b).map(drop)),
            ("f64", |b| from_beve::<f64>(b).map(drop)),
            ("Number", |b| from_beve::<Number>(b).map(drop)),
        ],
        header::TY_STRING => vec![
            ("String", |b| from_beve::<String>(b).map(drop)),
            ("&str", |b| from_beve::<&str>(b).map(drop)),
            ("Cow<str>", |b| from_beve::<Cow<str>>(b).map(drop)),
            ("Option<String>", |b| {
                from_beve::<Option<String>>(b).map(drop)
            }),
            ("enum", |b| from_beve::<Named>(b).map(drop)),
        ],
        header::TY_OBJECT => vec![
            ("struct", |b| from_beve::<Inner>(b).map(drop)),
            ("enum", |b| from_beve::<Named>(b).map(drop)),
            ("tagged enum", |b| from_beve::<Tagged>(b).map(drop)),
            ("HashMap<u64, _>", |b| {
                from_beve::<HashMap<u64, u8>>(b).map(drop)
            }),
            ("HashMap<i8, _>", |b| {
                from_beve::<HashMap<i8, u8>>(b).map(drop)
            }),
            ("HashMap<String, _>", |b| {
                from_beve::<HashMap<String, u8>>(b).map(drop)
            }),
        ],
        header::TY_TYPED_ARRAY => vec![
            ("Vec<u64>", |b| from_beve::<Vec<u64>>(b).map(drop)),
            ("Vec<f64>", |b| from_beve::<Vec<f64>>(b).map(drop)),
            ("Vec<bool>", |b| from_beve::<Vec<bool>>(b).map(drop)),
            ("Vec<String>", |b| from_beve::<Vec<String>>(b).map(drop)),
            ("Vec<u8>", |b| from_beve::<Vec<u8>>(b).map(drop)),
            ("&[u8]", |b| from_beve::<&[u8]>(b).map(drop)),
            ("Cow<[f64]>", |b| from_beve::<Cow<[f64]>>(b).map(drop)),
        ],
        header::TY_GENERIC_ARRAY => vec![
            ("Vec<Value>", |b| from_beve::<Vec<Value>>(b).map(drop)),
            ("Vec<u64>", |b| from_beve::<Vec<u64>>(b).map(drop)),
        ],
        // An extension's kind is its id. A delimiter is no kind at all; see
        // `a_delimiter_is_not_a_value_anywhere_one_belongs`.
        header::TY_EXTENSION => match header::ext_id(h) {
            header::EXT_COMPLEX => vec![
                ("Complex", |b| from_beve::<Complex<f64>>(b).map(drop)),
                ("Vec<Complex>", |b| {
                    from_beve::<Vec<Complex<f64>>>(b).map(drop)
                }),
            ],
            header::EXT_MATRIX => vec![("Matrix", |b| {
                from_beve::<structio::Matrix<f64>>(b).map(drop)
            })],
            _ => vec![],
        },
        _ => vec![],
    }
}

/// The same bytes as an unknown member's value, stepped over, with the offset
/// taken back to the value's own, the member's key being the four bytes in
/// front of it.
fn skipped(value: &[u8]) -> structio::Result<()> {
    let doc = [&[header::OBJECT, 1 << 2, 1 << 2, b'z'][..], value].concat();
    from_beve_with::<SkipUnknown, Inner>(&doc)
        .map(drop)
        .map_err(|e| structio::Error {
            index: e.index - 4,
            ..e
        })
}

/// Steps over whatever value is there, so that what a framer makes of a value
/// is not mixed up with what some particular type makes of it.
#[derive(Default)]
struct Any;

impl<'de> beve::Read<'de> for Any {
    fn read<O: structio::Options>(
        &mut self,
        r: &mut beve::Reader<'de, O>,
    ) -> Result<(), ErrorCode> {
        r.skip_value()
    }
}

/// What the two framers make of `doc`: `Documents::values` over it, and
/// `Documents::array` over it as the one element of a generic array, with the
/// latter's offsets taken back to the document's own. `Ok` is how many values
/// came out, and `Err` the first refusal.
fn framed(doc: &[u8]) -> [(&'static str, structio::Result<usize>); 2] {
    let element = [&[header::GENERIC_ARRAY, 1 << 2][..], doc].concat();
    let array = drain(beve::Documents::array(&element)).map_err(|e| structio::Error {
        index: e.index - 2,
        ..e
    });
    [
        ("Documents::values", drain(beve::Documents::values(doc))),
        ("Documents::array", array),
    ]
}

/// How many values `docs` hands out, or the first refusal.
fn drain(mut docs: beve::Documents<&[u8]>) -> structio::Result<usize> {
    let mut n = 0;
    while let Some(item) = docs.next_value::<Any>() {
        if let Err(e) = item {
            return Err(*e.as_parse().expect("a slice has no I/O to fail"));
        }
        n += 1;
    }
    Ok(n)
}

#[test]
fn every_walk_agrees_with_the_validator_on_every_header() {
    // Swept over every header byte, with a payload of a zero count, with one
    // of a single zero element, so that a refusal on the header is never
    // mistaken for one about what follows it, with one of a single element
    // whose byte sets bit 1, which is padding to a packed-boolean array, and
    // with one that is an empty array of bytes padded by one, which is more
    // than the aligned form allows. Every walk that takes whatever is there
    // accepts what the validator accepts and refuses what it refuses, with its
    // code at its offset. A reader that wanted some other kind reports the
    // mismatch, `ExpectedString` and the like; one that wanted this kind has
    // met a header of the right type that is no value at all, and says so as
    // the validator does.
    //
    // Three differences are by design. A walk that decodes refuses what it
    // cannot decode as soon as it knows: a 128-bit float or the deprecated
    // type tag on the header, and an undefined matrix layout on its byte. The
    // validator steps over all three and may find something wrong further on.
    // A pointer read never looks past the value it names. And a framer reports
    // every refusal against the value's start rather than past its header.
    let everyone: [(&str, Walk); 4] = [
        ("Value", |b| from_beve::<Value>(b).map(drop)),
        ("pointer Value", |b| {
            beve::from_slice_at::<Value>(b, "").map(drop)
        }),
        ("transcode", |b| beve_to_json(b).map(drop)),
        ("skip", skipped),
    ];
    let padded_bytes = [
        header::header(header::TY_TYPED_ARRAY, header::CAT_UNSIGNED, 0),
        0,
        1,
        0,
    ];
    let (mut refused, mut padding) = (0, 0);
    for h in 0..=255u8 {
        for tail in [
            &[0u8][..],
            &[1 << 2, 0, 0, 0],
            &[1 << 2, 1 << 1],
            &padded_bytes,
        ] {
            let doc = [&[h][..], tail].concat();
            let verdict = validate_beve(&doc);
            let at = |r: structio::Result<()>| r.map_err(|e| (e.code, e.index));
            for (name, walk) in &everyone {
                match (at(verdict), at(walk(&doc))) {
                    (v, e) if v == e => {}
                    (
                        v,
                        Err((ErrorCode::UnsupportedFeature | ErrorCode::InvalidMatrixLayout, e)),
                    ) if *name != "skip" && v.err().is_none_or(|(_, v)| v >= e) => {}
                    (Err((ErrorCode::TrailingContent, _)), Ok(())) if *name == "pointer Value" => {}
                    (v, e) => panic!("{name}, {doc:02x?}: validate {v:?}, walk {e:?}"),
                }
            }
            let Err(v) = verdict else {
                for (name, framed) in framed(&doc) {
                    assert_eq!(framed.map_err(|e| e.code), Ok(1), "{name}, {doc:02x?}");
                }
                continue;
            };
            padding += usize::from(v.code == ErrorCode::InvalidPadding);
            if v.code != ErrorCode::InvalidHeader {
                continue;
            }
            refused += 1;
            for (name, walk) in readers_of(h) {
                let e = walk(&doc).expect_err(name);
                assert_eq!((e.code, e.index), (v.code, v.index), "{name}, {doc:02x?}");
            }
            for (name, framed) in framed(&doc) {
                // In front of a document, a delimiter separates it from the
                // one before, which is what `values` takes it for.
                if h == header::DELIMITER && name == "Documents::values" {
                    continue;
                }
                let framed = framed.map_err(|e| (e.code, e.index));
                assert_eq!(framed, Err((v.code, 0)), "{name}, {doc:02x?}");
            }
        }
    }
    assert!(refused > 0);
    // The packed-boolean array setting a bit past its one element, and the
    // aligned array padded past its width.
    assert_eq!(padding, 2);
}

#[test]
fn a_header_with_an_unspecified_bit_set_is_not_a_value_in_any_walk() {
    // The specification gives a string and a generic array no `sub` and no
    // `count`, and a string-keyed object no `count`, and requires every bit it
    // leaves unspecified to be zero. Read as zero, each set bit would be one
    // more spelling of the same value, which a document that is compared,
    // hashed or signed byte for byte cannot have. So each of the 69 is refused
    // on its header, in every walk, wherever it stands: as the document, as an
    // element, as the tag an internally tagged enum dispatches on, and as a
    // container a pointer passes through. "A" is `Tagged`'s one variant.
    let strings = (0..32).map(|bits| {
        let h = (bits << 3) | header::TY_STRING;
        (h, vec![h, 1 << 2, b'A'], None)
    });
    let arrays = (0..32).map(|bits| {
        let h = (bits << 3) | header::TY_GENERIC_ARRAY;
        (h, vec![h, 1 << 2, header::NULL], Some("/0"))
    });
    let objects = (0..8).map(|count| {
        let h = (count << 5) | header::OBJECT;
        (h, vec![h, 1 << 2, 1 << 2, b'A', header::NULL], Some("/A"))
    });

    let mut spellings = 0;
    for (h, doc, pointer) in strings.chain(arrays).chain(objects) {
        let canonical = h == header::ty(h);
        let element = [&[header::GENERIC_ARRAY, 1 << 2][..], &doc].concat();
        // Each with the offset just past the header, wherever it stands.
        let mut walks: Vec<(&str, structio::Result<()>, usize)> = vec![
            ("validate", validate_beve(&doc), 1),
            ("Value", from_beve::<Value>(&doc).map(drop), 1),
            (
                "pointer Value",
                from_beve_at::<Value>(&doc, "").map(drop),
                1,
            ),
            ("transcode", beve_to_json(&doc).map(drop), 1),
            ("skip", skipped(&doc), 1),
            ("element, validate", validate_beve(&element), 3),
            ("element, Value", from_beve::<Value>(&element).map(drop), 3),
            ("element, transcode", beve_to_json(&element).map(drop), 3),
        ];
        if let Some(pointer) = pointer {
            let through = from_beve_at::<Value>(&doc, pointer).map(drop);
            walks.push(("through a pointer", through, 1));
        }
        if header::ty(h) == header::TY_STRING {
            let tag = [&[header::OBJECT, 1 << 2, 4 << 2][..], b"kind", &doc].concat();
            walks.push(("internal tag", from_beve::<Tagged>(&tag).map(drop), 8));
        }
        // Against the value's start, the element's being two bytes in.
        let mut framers: Vec<_> = framed(&doc)
            .into_iter()
            .map(|(name, r)| (name, r, 0))
            .chain(framed(&element).into_iter().map(|(name, r)| (name, r, 2)))
            .collect();
        // An array is also one `Documents::array` can take the elements of.
        if header::ty(h) == header::TY_GENERIC_ARRAY {
            let outer = drain(beve::Documents::array(&doc));
            framers.push(("Documents::array, outer", outer, 0));
        }

        if canonical {
            for (name, r, _) in walks {
                r.unwrap_or_else(|e| panic!("{name}, {h:#04x}: {e:?}"));
            }
            for (name, r, _) in framers {
                assert_eq!(r.map_err(|e| e.code), Ok(1), "{name}, {h:#04x}");
            }
            continue;
        }
        spellings += 1;
        // And every reader of the kind, asked only here because not all of
        // them can read what the canonical documents hold.
        walks.extend(
            readers_of(h)
                .into_iter()
                .map(|(name, walk)| (name, walk(&doc), 1)),
        );
        for (name, r, at) in walks {
            let e = r.expect_err(name);
            let want = (ErrorCode::InvalidHeader, at);
            assert_eq!((e.code, e.index), want, "{name}, {h:#04x}");
        }
        for (name, r, at) in framers {
            let e = r.expect_err(name);
            let want = (ErrorCode::InvalidHeader, at);
            assert_eq!((e.code, e.index), want, "{name}, {h:#04x}");
        }
    }
    assert_eq!(spellings, 69);

    // The fourth key type is not defined at any width, so an object of it is
    // no value at all rather than one whose keys some reader does not take.
    for h in (0..8).map(|count| header::header(header::TY_OBJECT, 3, count)) {
        let e = validate_beve(&[h, 0]).unwrap_err();
        assert_eq!((e.code, e.index), (ErrorCode::InvalidHeader, 1), "{h:#04x}");
    }
}

/// `[bool; N]` for each count the padding test sweeps, `N` being the index.
/// A table rather than a const generic, `Default` for an array being
/// implemented one length at a time.
macro_rules! fixed_bools {
    ($($n:literal)*) => {
        [$(|b| from_beve::<[bool; $n]>(b).map(drop)),*]
    };
}
const FIXED_BOOLS: [Walk; 18] = fixed_bools!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17);

/// `doc` pushed into a `Feed` one byte at a time, in either mode: how many
/// values came out, and the first refusal.
fn dribbled(array: bool, doc: &[u8]) -> (usize, structio::Result<()>) {
    let mut feed = if array {
        beve::Feed::array()
    } else {
        beve::Feed::values()
    };
    let mut n = 0;
    for i in 0..=doc.len() {
        match doc.get(i) {
            Some(b) => feed.push(std::slice::from_ref(b)),
            None => feed.end(),
        }
        while let Some(item) = feed.next_value::<Any>() {
            match item {
                Ok(_) => n += 1,
                Err(e) => return (n, Err(*e.as_parse().expect("a feed has no I/O"))),
            }
        }
    }
    (n, Ok(()))
}

#[test]
fn a_packed_boolean_array_with_padding_set_is_not_a_value_in_any_walk() {
    // The specification requires the high bits of a packed-boolean array's
    // last byte, the ones past its last element, to be zero. Read as zero, a
    // set one would give the same array several encodings, so every walk
    // refuses it as `InvalidPadding`, just past the payload's last byte: as
    // the document, as an element, through a pointer at any index, and
    // stepped over. A framer reports it at the value's start, as it reports
    // everything, and a top-level array streamed an element at a time hands
    // out every element before the last and then refuses at the last byte.
    let mut spellings = 0;
    for (n, fixed) in FIXED_BOOLS.iter().enumerate() {
        let bools: Vec<bool> = (0..n).map(|i| i % 3 != 1).collect();
        let canonical = to_beve(&bools);
        // The header, a one-byte count, and the payload.
        let end = 2 + n.div_ceil(8);
        assert_eq!(canonical.len(), end, "{n}");

        let spelled = (n & 7 != 0)
            .then(|| (n & 7..8).map(|bit| (Some(bit), canonical.clone())))
            .into_iter()
            .flatten();
        for (bit, mut doc) in std::iter::once((None, canonical.clone())).chain(spelled) {
            if let Some(bit) = bit {
                doc[end - 1] |= 1 << bit;
            }
            let element = [&[header::GENERIC_ARRAY, 1 << 2][..], &doc].concat();
            let mut walks: Vec<(String, structio::Result<()>, usize)> = vec![
                ("validate".into(), validate_beve(&doc), end),
                ("Value".into(), from_beve::<Value>(&doc).map(drop), end),
                (
                    "pointer Value".into(),
                    from_beve_at::<Value>(&doc, "").map(drop),
                    end,
                ),
                ("transcode".into(), beve_to_json(&doc).map(drop), end),
                ("skip".into(), skipped(&doc), end),
                (
                    "Vec<bool>".into(),
                    from_beve::<Vec<bool>>(&doc).map(drop),
                    end,
                ),
                (
                    "VecDeque<bool>".into(),
                    from_beve::<VecDeque<bool>>(&doc).map(drop),
                    end,
                ),
                (format!("[bool; {n}]"), fixed(&doc), end),
                ("element, validate".into(), validate_beve(&element), end + 2),
                (
                    "element, Value".into(),
                    from_beve::<Value>(&element).map(drop),
                    end + 2,
                ),
                (
                    "element, transcode".into(),
                    beve_to_json(&element).map(drop),
                    end + 2,
                ),
                (
                    "element, Vec<Vec<bool>>".into(),
                    from_beve::<Vec<Vec<bool>>>(&element).map(drop),
                    end + 2,
                ),
                (
                    "through a pointer".into(),
                    from_beve_at::<Value>(&element, "/0").map(drop),
                    end + 2,
                ),
            ];
            for i in 0..n {
                let at = from_beve_at::<bool>(&doc, &format!("/{i}")).map(drop);
                walks.push((format!("pointer /{i}"), at, end));
                let at = from_beve_at::<bool>(&element, &format!("/0/{i}")).map(drop);
                walks.push((format!("pointer /0/{i}"), at, end + 2));
            }
            // Against the value's start, the element's being two bytes in.
            let mut framers: Vec<(String, structio::Result<usize>, usize)> = framed(&doc)
                .into_iter()
                .map(|(name, r)| (name.into(), r, 0))
                .chain(
                    framed(&element)
                        .into_iter()
                        .map(|(name, r)| (format!("element, {name}"), r, 2)),
                )
                .collect();
            let (count, r) = dribbled(false, &doc);
            framers.push(("Feed::values, dribbled".into(), r.map(|()| count), 0));

            // The array's own elements, handed out as they arrive.
            let whole = drain(beve::Documents::array(&doc));
            let (streamed, dribble) = dribbled(true, &doc);

            let Some(bit) = bit else {
                for (name, r, _) in walks {
                    r.unwrap_or_else(|e| panic!("{name}, {n}: {e:?}"));
                }
                for (name, r, _) in framers {
                    assert_eq!(r.map_err(|e| e.code), Ok(1), "{name}, {n}");
                }
                assert_eq!(from_beve::<Vec<bool>>(&doc).unwrap(), bools);
                assert_eq!(whole.map_err(|e| e.code), Ok(n), "{n}");
                assert_eq!((streamed, dribble.map_err(|e| e.code)), (n, Ok(())));
                continue;
            };
            spellings += 1;
            let want = |at| Err((ErrorCode::InvalidPadding, at));
            for (name, r, at) in walks {
                let got = r.map_err(|e| (e.code, e.index));
                assert_eq!(got, want(at), "{name}, {n}, bit {bit}");
            }
            for (name, r, at) in framers {
                let got = r.map(drop).map_err(|e| (e.code, e.index));
                assert_eq!(got, want(at), "{name}, {n}, bit {bit}");
            }
            let whole = whole.map(drop).map_err(|e| (e.code, e.index));
            assert_eq!(whole, want(end - 1), "Documents::array, {n}, bit {bit}");
            let dribble = dribble.map_err(|e| (e.code, e.index));
            assert_eq!(
                (streamed, dribble),
                (n - 1, want(end - 1)),
                "Feed::array, {n}, bit {bit}"
            );
        }
    }
    // Seven widths of padding in each of 1..8, 9..16 and 17.
    assert_eq!(spellings, 28 + 28 + 7);
}

/// The typed reads of one element type, each over a whole document: the
/// element type's header, and each read's name and walk.
type TypedReads = (u8, [(&'static str, Walk); 5]);

/// [`TypedReads`] for each numeric type an aligned block can hold: as a
/// vector, which takes a matching block in one copy and any other element by
/// element; borrowed, which points into the input where it can; as a fixed
/// array; through a pointer to its first element; and read from a stream.
macro_rules! typed_reads {
    ($($t:ty)*) => {
        [$((
            <$t as beve::NumericBytes>::ELEMENT,
            [
                (concat!("Vec<", stringify!($t), ">"), |b| from_beve::<Vec<$t>>(b).map(drop)),
                (concat!("Cow<[", stringify!($t), "]>"), |b| {
                    from_beve::<Cow<[$t]>>(b).map(drop)
                }),
                (concat!("[", stringify!($t), "; 1]"), |b| from_beve::<[$t; 1]>(b).map(drop)),
                (concat!("pointer /0 as ", stringify!($t)), |b| {
                    from_beve_at::<$t>(b, "/0").map(drop)
                }),
                (concat!("read_beve_array_into::<", stringify!($t), ">"), |b| {
                    read_beve_array_into::<$t, _>(&mut Vec::new(), b)
                        .map_err(|e| *e.as_parse().expect("a slice has no I/O to fail"))
                }),
            ],
        )),*]
    };
}
const TYPED_READS: [TypedReads; 12] = typed_reads!(u8 i8 u16 i16 u32 i32 f32 u64 i64 f64 u128 i128);

#[test]
fn an_aligned_array_padded_past_its_alignment_is_not_a_value_in_any_walk() {
    // The specification bounds an aligned typed array's `PADDING_LENGTH` to
    // `0..alignment`, the alignment being the element's width, and leaves the
    // padding's contents unspecified. So every walk takes a length below the
    // width, whatever the padding holds, and refuses one at or past it as
    // `InvalidPadding`, just past the length byte: as the document, as an
    // element, through a pointer, stepped over, read as every numeric type,
    // and read from a stream. The length is refused before the padding it
    // announces is looked for, so a document cut short behind it is refused
    // for the same reason. A framer reports it at the value's start, as it
    // reports everything, a top-level array streamed an element at a time
    // included, its preamble being refused before any element goes out.
    let elements = [header::CAT_FLOAT, header::CAT_SIGNED, header::CAT_UNSIGNED]
        .into_iter()
        .flat_map(|cat| (0..8).map(move |count| (cat, count)))
        .filter_map(|(cat, count)| {
            let width = header::byte_width(cat, count)?;
            Some((header::header(header::TY_TYPED_ARRAY, cat, count), width))
        });
    // A 128-bit float has no Rust type to decode into, so the walks that
    // decode refuse it on its header whatever its padding says.
    let f128 = header::header(header::TY_TYPED_ARRAY, header::CAT_FLOAT, 4);

    let (mut accepted, mut refused) = (0, 0);
    let mut widths = Vec::new();
    for (inner, width) in elements {
        widths.push(width);
        // Past what each width allows, sampled at the edges and at the most a
        // byte can say.
        let over = [width, width + 1, 15, 16, 17, 127, 128, 254, 255]
            .into_iter()
            .filter(|&pad| pad >= width);
        for pad in (0..width).chain(over) {
            // One element, zero, behind padding that is not zero, which is
            // there to be ignored.
            let whole = [
                &[header::ALIGNED_ARRAY, inner, 1 << 2, pad as u8][..],
                &vec![0xAA; pad],
                &vec![0; width],
            ]
            .concat();
            let ok = pad < width;
            // Refused on the length alone, before the padding is looked for.
            let cut = (!ok).then(|| whole[..4].to_vec());
            for doc in std::iter::once(whole.clone()).chain(cut) {
                let element = [&[header::GENERIC_ARRAY, 1 << 2][..], &doc].concat();
                let mut walks: Vec<(String, structio::Result<()>, usize)> = vec![
                    ("validate".into(), validate_beve(&doc), 4),
                    ("skip".into(), skipped(&doc), 4),
                    ("element, validate".into(), validate_beve(&element), 6),
                ];
                if inner != f128 || !ok {
                    walks.extend([
                        ("Value".into(), from_beve::<Value>(&doc).map(drop), 4),
                        (
                            "pointer Value".into(),
                            from_beve_at::<Value>(&doc, "").map(drop),
                            4,
                        ),
                        ("transcode".into(), beve_to_json(&doc).map(drop), 4),
                        (
                            "element, Value".into(),
                            from_beve::<Value>(&element).map(drop),
                            6,
                        ),
                        (
                            "element, transcode".into(),
                            beve_to_json(&element).map(drop),
                            6,
                        ),
                        (
                            "through a pointer".into(),
                            from_beve_at::<Value>(&element, "/0").map(drop),
                            6,
                        ),
                    ]);
                }
                // Refused, every typed read refuses it, whatever it wanted.
                // Accepted, the ones that want this element type read it.
                for (element_type, reads) in TYPED_READS {
                    if ok && element_type != header::element_of(inner) {
                        continue;
                    }
                    for (name, read) in reads {
                        walks.push((name.into(), read(&doc), 4));
                    }
                }
                if !ok {
                    walks.extend(
                        readers_of(header::ALIGNED_ARRAY)
                            .into_iter()
                            .map(|(name, walk)| (name.into(), walk(&doc), 4)),
                    );
                }
                // Against the value's start, the element's being two bytes in.
                let mut framers: Vec<(String, structio::Result<usize>, usize)> = framed(&doc)
                    .into_iter()
                    .map(|(name, r)| (name.into(), r, 0))
                    .chain(
                        framed(&element)
                            .into_iter()
                            .map(|(name, r)| (format!("element, {name}"), r, 2)),
                    )
                    .collect();
                let (count, r) = dribbled(false, &doc);
                framers.push(("Feed::values, dribbled".into(), r.map(|()| count), 0));
                // The array's own elements, handed out as they arrive.
                let whole = drain(beve::Documents::array(&doc));
                framers.push(("Documents::array, outer".into(), whole, 0));
                let (count, r) = dribbled(true, &doc);
                framers.push(("Feed::array, dribbled".into(), r.map(|()| count), 0));

                let label = format!("{inner:#04x}, padding {pad}, {} bytes", doc.len());
                if ok {
                    accepted += 1;
                    for (name, r, _) in walks {
                        r.unwrap_or_else(|e| panic!("{name}, {label}: {e:?}"));
                    }
                    for (name, r, _) in framers {
                        assert_eq!(r.map_err(|e| e.code), Ok(1), "{name}, {label}");
                    }
                    continue;
                }
                refused += 1;
                for (name, r, at) in walks {
                    let got = r.map_err(|e| (e.code, e.index));
                    assert_eq!(got, Err((ErrorCode::InvalidPadding, at)), "{name}, {label}");
                }
                for (name, r, at) in framers {
                    let got = r.map_err(|e| (e.code, e.index));
                    assert_eq!(got, Err((ErrorCode::InvalidPadding, at)), "{name}, {label}");
                }
            }
        }
    }
    // Five float types, two of them two bytes wide, and five integer widths of
    // each signedness, the one-byte integers taking no padding at all. Every
    // length below each width was taken, and each refused one twice, whole
    // and cut short.
    widths.sort_unstable();
    assert_eq!(widths, [1, 1, 2, 2, 2, 2, 4, 4, 4, 8, 8, 8, 16, 16, 16]);
    assert_eq!(accepted, widths.iter().sum::<usize>());
    assert!(refused > 2 * widths.len());
}

#[test]
fn a_matrix_holding_an_aligned_array_padded_past_its_alignment_is_not_a_value() {
    // A matrix's data is an ordinary value, so its aligned form is refused
    // exactly as the same array standing alone is, just past its length byte.
    let m = Matrix::new(MatrixLayout::RowMajor, vec![1, 2], vec![1.5f64, -2.0]).unwrap();
    let canonical = to_beve_aligned(&m);
    let f64s = header::header(header::TY_TYPED_ARRAY, header::CAT_FLOAT, 3);
    let marker = canonical
        .windows(2)
        .position(|w| w == [header::ALIGNED_ARRAY, f64s])
        .expect("the data is in the aligned form");
    // The marker, the element header and a one-byte count, then the length.
    let length = marker + 3;
    let data = &canonical[length + 1 + usize::from(canonical[length])..];
    for pad in 0..=255usize {
        let doc = [&canonical[..length], &[pad as u8], &vec![0xAA; pad], data].concat();
        let walks = [
            ("validate", validate_beve(&doc)),
            ("Matrix", from_beve::<Matrix<f64>>(&doc).map(drop)),
            ("Value", from_beve::<Value>(&doc).map(drop)),
            ("transcode", beve_to_json(&doc).map(drop)),
            ("skip", skipped(&doc)),
        ];
        let framers = framed(&doc);
        if pad < 8 {
            assert_eq!(from_beve::<Matrix<f64>>(&doc).unwrap(), m, "{pad}");
            for (name, r) in walks {
                r.unwrap_or_else(|e| panic!("{name}, padding {pad}: {e:?}"));
            }
            for (name, r) in framers {
                assert_eq!(r.map_err(|e| e.code), Ok(1), "{name}, padding {pad}");
            }
            continue;
        }
        for (name, r) in walks {
            let got = r.map_err(|e| (e.code, e.index));
            let want = Err((ErrorCode::InvalidPadding, length + 1));
            assert_eq!(got, want, "{name}, padding {pad}");
        }
        // A framer reports it at the start of the array, the value it refused.
        for (name, r) in framers {
            let got = r.map_err(|e| (e.code, e.index));
            assert_eq!(
                got,
                Err((ErrorCode::InvalidPadding, marker)),
                "{name}, padding {pad}"
            );
        }
    }
}

#[test]
fn an_undefined_extension_is_reported_as_unsupported_not_as_malformed() {
    // Extension id 4 has no meaning, so its extent is unknown and nothing
    // after it can be located.
    let bad = [header::header(header::TY_EXTENSION, 0, 0) | (4 << 3)];
    assert_eq!(
        validate_beve(&bad).unwrap_err().code,
        ErrorCode::UnsupportedFeature
    );
}

#[test]
fn nesting_past_the_limit_is_rejected() {
    // Generic arrays nested deeper than the reader will descend.
    let deep: Vec<u8> = std::iter::repeat_n(header::GENERIC_ARRAY, 400)
        .flat_map(|h| [h, 1 << 2])
        .chain([header::NULL])
        .collect();
    assert_eq!(
        validate_beve(&deep).unwrap_err().code,
        ErrorCode::ExceededMaxDepth
    );
}

#[test]
fn at_the_nesting_limit_every_walk_refuses_for_the_same_reason() {
    // `MAX_DEPTH` containers around a container that is one level too deep
    // and has an undefined header too, so every walk has to give the same one
    // of those two reasons. A typed array is charged its level before its
    // element type is looked at, as reading a sequence charges it, so for one
    // of an undefined element width the answer is the depth. A generic array's
    // header is settled before its level, as an object's key kind is, so for
    // one with an unspecified bit set the answer is the header. Either is
    // reported just past the header, or at the value's start by a framer.
    struct Nest(u32);
    impl<'de> beve::Read<'de> for Nest {
        fn read<O: structio::Options>(
            &mut self,
            r: &mut beve::Reader<'de, O>,
        ) -> Result<(), ErrorCode> {
            if self.0 == 0 {
                return beve::Read::read(&mut Vec::<f64>::new(), r);
            }
            let inner = self.0 - 1;
            r.read_seq(|r, _| Nest(inner).read(r)).map(drop)
        }
    }

    let limit = beve::reader::MAX_DEPTH;
    let chain = [header::GENERIC_ARRAY, 1 << 2].repeat(limit as usize);
    let at = chain.len();
    for (innermost, code) in [
        (
            header::array_of(header::CAT_FLOAT, 5),
            ErrorCode::ExceededMaxDepth,
        ),
        (header::GENERIC_ARRAY | 1 << 3, ErrorCode::InvalidHeader),
    ] {
        let doc = [&chain[..], &[innermost, 0]].concat();
        for (name, r) in [
            ("from_beve", from_beve::<Value>(&doc).map(drop)),
            ("typed", beve::read_into(&mut Nest(limit), &doc)),
            ("validate_beve", validate_beve(&doc)),
            ("beve_to_json", beve_to_json(&doc).map(drop)),
            ("from_beve_at", from_beve_at::<Value>(&doc, "").map(drop)),
        ] {
            let r = r.map_err(|e| (e.code, e.index));
            assert_eq!(r, Err((code, at + 1)), "{name}, {innermost:#04x}");
        }
        for (name, r) in [
            ("Documents::values", drain(beve::Documents::values(&doc))),
            ("Documents::array", drain(beve::Documents::array(&doc))),
        ] {
            let r = r.map_err(|e| (e.code, e.index));
            assert_eq!(r, Err((code, at)), "{name}, {innermost:#04x}");
        }
    }
}

#[test]
fn both_framers_measure_an_element_at_the_depth_it_sits_at() {
    // `Documents::array` hands out the elements of the outermost array, which
    // is a container like any other, so an element sits a level down in the
    // document. Framed from zero, an element could be one level deeper than
    // every other walk allows, and the two framers disagreed about the same
    // bytes. One past the limit is refused by both, at the start of the
    // container that crossed it, and one at the limit is framed by both.
    let limit = beve::reader::MAX_DEPTH as usize;
    let arrays = |n: usize| [header::GENERIC_ARRAY, 1 << 2].repeat(n);
    let undefined = [header::array_of(header::CAT_FLOAT, 5), 0];
    let empty = [header::GENERIC_ARRAY, 0];
    for (name, doc, refused) in [
        (
            "past the limit, then an undefined typed array",
            [arrays(limit + 1), undefined.to_vec()].concat(),
            Some(2 * limit),
        ),
        (
            "past the limit with an empty array",
            [arrays(limit), empty.to_vec()].concat(),
            Some(2 * limit),
        ),
        (
            "at the limit",
            [arrays(limit - 1), empty.to_vec()].concat(),
            None,
        ),
    ] {
        assert_eq!(validate_beve(&doc).is_ok(), refused.is_none(), "{name}");
        for (mode, r) in [
            ("values", drain(beve::Documents::values(&doc))),
            ("array", drain(beve::Documents::array(&doc))),
        ] {
            let want = match refused {
                Some(at) => Err((ErrorCode::ExceededMaxDepth, at)),
                None => Ok(1),
            };
            assert_eq!(r.map_err(|e| (e.code, e.index)), want, "{mode}, {name}");
        }
    }
}

#[test]
fn a_typed_array_costs_the_level_reading_charges_it() {
    // A typed array's elements are scalars, so stepping over one never
    // recurses, and it is tempting to let it through free. `read_seq` charges
    // it a level all the same, and a typed array is where the deepest value in
    // a real document tends to sit, so a validator that did not charge it
    // would accept, one level down, exactly the documents reading refuses.
    //
    // `Deep` is the shape a `Vec<Vec<..<Vec<u8>>>>` reads with: one `read_seq`
    // per level, which is what a nested type would have generated.
    struct Deep(u32);
    impl<'de> beve::Read<'de> for Deep {
        fn read<O: structio::Options>(
            &mut self,
            r: &mut beve::Reader<'de, O>,
        ) -> Result<(), ErrorCode> {
            if self.0 == 0 {
                return beve::Read::read(&mut 0u8, r);
            }
            let inner = self.0 - 1;
            r.read_seq(|r, _| Deep(inner).read(r)).map(|_| ())
        }
    }

    // `outer` generic arrays around one typed `u8` array of one element, so
    // the sequences to descend number `outer + 1`.
    let doc = |outer: u32| -> Vec<u8> {
        let mut b: Vec<u8> = std::iter::repeat_n(header::GENERIC_ARRAY, outer as usize)
            .flat_map(|h| [h, 1 << 2])
            .collect();
        b.extend_from_slice(&[header::array_of(header::CAT_UNSIGNED, 0), 1 << 2, 7]);
        b
    };

    for outer in [beve::reader::MAX_DEPTH - 1, beve::reader::MAX_DEPTH] {
        let bytes = doc(outer);
        let read = beve::read_into(&mut Deep(outer + 1), &bytes).map_err(|e| e.code);
        let validated = validate_beve(&bytes).map_err(|e| e.code);
        assert_eq!(
            validated.is_ok(),
            read.is_ok(),
            "{} sequences: validate={validated:?} read={read:?}",
            outer + 1
        );
    }
}

/// A chain of `{"next": ..}` objects ending in `{"data": ..}`, one per
/// destination a typed array can be read into, so reading can reach the leaf at
/// any depth.
macro_rules! chains {
    ($($name:ident: $data:ty),* $(,)?) => {$(
        #[derive(Default)]
        struct $name {
            next: Option<Box<$name>>,
            data: $data,
        }
        structio::object!($name { next, data });
        impl Leaf for $name {
            type Data = $data;
        }
    )*};
}

/// A chain type's leaf, which is what a pointer to `/data` reads.
trait Leaf: beve::ReadOwned {
    type Data: beve::ReadOwned;
}

chains! {
    VecF64: Vec<f64>,
    VecF32: Vec<f32>,
    VecU32: Vec<u32>,
    VecI64: Vec<i64>,
    VecU8: Vec<u8>,
    DequeF64: VecDeque<f64>,
    ArrayF64: [f64; 1],
    VecBool: Vec<bool>,
    VecString: Vec<String>,
    VecComplex: Vec<Complex<f64>>,
}

/// The destinations that borrow, which take the other two whole-block reads.
#[derive(Default)]
struct CowF64<'a> {
    next: Option<Box<CowF64<'a>>>,
    data: Cow<'a, [f64]>,
}
structio::object!(['a] CowF64<'a> { next, data });

#[derive(Default)]
struct BytesU8<'a> {
    next: Option<Box<BytesU8<'a>>>,
    data: &'a [u8],
}
structio::beve_object!(['a] BytesU8<'a> { next, data });

#[test]
fn every_walk_takes_a_typed_array_leaf_to_the_same_depth() {
    // A typed array can be read element by element, copied whole, or borrowed
    // whole as a `Cow` or a byte slice. Every one of those has to charge the
    // level validating, transcoding and framing charge it, or a document one
    // level from the limit reads and is then refused by the others, framing
    // included, which ends a stream at the first such record. So each
    // destination is taken to its deepest document by every walk, and all five
    // have to agree at every depth, the fifth being a pointer to the leaf,
    // which walks the chain rather than reading it and so has to charge the
    // levels it passes through itself. The element path is here for all three
    // of its forms, numbers, packed booleans and strings. A complex array is
    // the control: it is the one sequence no walk charges, so the bulk copy
    // that takes it must not charge it either.
    let limit = beve::reader::MAX_DEPTH as usize;
    // The chain, the leaf's object and the array; a complex array costs none.
    let typed = Some(limit - 2);
    let complex = Some(limit - 1);

    let f64s = to_beve(&vec![1.5f64]);
    let f32s = to_beve(&vec![1.5f32]);
    let u32s = to_beve(&vec![7u32]);
    let i64s = to_beve(&vec![-7i64]);
    let u8s = to_beve(&vec![7u8]);
    let bools = to_beve(&vec![true]);
    let strings = to_beve(&vec!["a".to_string()]);
    let pairs = to_beve(&vec![Complex::new(1.5f64, -1.5)]);
    let cow = Reads {
        whole: |d| from_beve::<CowF64>(d).is_ok(),
        at: |d, p| from_beve_at::<Cow<[f64]>>(d, p).is_ok(),
    };
    let bytes = Reads {
        whole: |d| from_beve::<BytesU8>(d).is_ok(),
        at: |d, p| from_beve_at::<&[u8]>(d, p).is_ok(),
    };

    assert_eq!(deepest("Vec<f64>", &f64s, reads::<VecF64>()), typed);
    assert_eq!(deepest("Vec<f32>", &f32s, reads::<VecF32>()), typed);
    assert_eq!(deepest("Vec<u32>", &u32s, reads::<VecU32>()), typed);
    assert_eq!(deepest("Vec<i64>", &i64s, reads::<VecI64>()), typed);
    assert_eq!(deepest("Vec<u8>", &u8s, reads::<VecU8>()), typed);
    assert_eq!(deepest("Cow<[f64]>", &f64s, cow), typed);
    assert_eq!(deepest("&[u8]", &u8s, bytes), typed);
    assert_eq!(deepest("VecDeque<f64>", &f64s, reads::<DequeF64>()), typed);
    assert_eq!(deepest("[f64; 1]", &f64s, reads::<ArrayF64>()), typed);
    assert_eq!(deepest("Vec<bool>", &bools, reads::<VecBool>()), typed);
    assert_eq!(
        deepest("Vec<String>", &strings, reads::<VecString>()),
        typed
    );
    let deepest_complex = deepest("Vec<Complex<f64>>", &pairs, reads::<VecComplex>());
    assert_eq!(deepest_complex, complex);
}

/// Two ways to read a chain's leaf: the whole chain, and the leaf alone
/// through the pointer that names it.
struct Reads {
    whole: fn(&[u8]) -> bool,
    at: fn(&[u8], &str) -> bool,
}

fn reads<T: Leaf>() -> Reads {
    Reads {
        whole: |d| from_beve::<T>(d).is_ok(),
        at: |d, p| from_beve_at::<T::Data>(d, p).is_ok(),
    }
}

/// The deepest chain around `{"data": array}` that `read` accepts whole,
/// having required validating, transcoding, framing and reading the leaf
/// through a pointer to accept exactly the same chains.
fn deepest(name: &str, array: &[u8], read: Reads) -> Option<usize> {
    let key = |name: &[u8; 4]| [&[header::OBJECT, 1 << 2, 4 << 2][..], name].concat();
    let limit = beve::reader::MAX_DEPTH as usize;
    // Every depth ordinarily; under Miri only the ones around the limit, which
    // is the only place the answer can change.
    let depths: Vec<usize> = if cfg!(miri) {
        vec![0, limit - 3, limit - 2, limit - 1, limit]
    } else {
        (0..=limit).collect()
    };

    let mut deepest = None;
    for n in depths {
        let mut doc = key(b"next").repeat(n);
        doc.extend_from_slice(&key(b"data"));
        doc.extend_from_slice(array);

        let reads = (read.whole)(&doc);
        let validates = validate_beve(&doc).is_ok();
        let transcodes = beve_to_json(&doc).is_ok();
        // The splitter's own verdict, which is how far it advanced: the reader
        // behind it applies the limit again from zero, so an error alone would
        // not say whose it was.
        let mut feed = beve::Feed::values();
        feed.push(&doc);
        feed.end();
        let _ = feed.next_value::<Value>();
        let frames = feed.offset() == doc.len();
        // The pointer walks the chain rather than reading it, so the levels it
        // passes through are charged by the walk itself.
        let pointed = (read.at)(&doc, &format!("{}/data", "/next".repeat(n)));

        assert_eq!(
            [validates, transcodes, frames, pointed],
            [reads; 4],
            "{name} under {n} containers: read {reads}, validate {validates}, \
             transcode {transcodes}, frame {frames}, pointer {pointed}"
        );
        if reads {
            deepest = Some(n);
        }
    }
    deepest
}

#[test]
fn a_whole_block_read_is_charged_a_level_whether_copied_or_borrowed() {
    // The destinations above reach the whole-block reads through their own
    // impls, and whether `Cow` borrows depends on where the document landed.
    // This asks each directly, at a depth set exactly, on a `u8` array, which
    // every address can be borrowed from. Past the limit the copy and the
    // borrow decline, and the element path they fall back to is what refuses;
    // the byte slice has no fallback and refuses on its own, with the same
    // error.
    #[derive(Clone, Copy)]
    enum Take {
        Borrowed,
        Copied,
        Bytes,
    }
    struct Deep<'a> {
        outer: u32,
        take: Take,
        took: &'a Cell<bool>,
    }
    impl<'de> beve::Read<'de> for Deep<'_> {
        fn read<O: structio::Options>(
            &mut self,
            r: &mut beve::Reader<'de, O>,
        ) -> Result<(), ErrorCode> {
            if self.outer > 0 {
                let (take, took) = (self.take, self.took);
                let outer = self.outer - 1;
                return r
                    .read_seq(|r, _| Deep { outer, take, took }.read(r))
                    .map(|_| ());
            }
            let mut out = Vec::<u8>::new();
            let took = match self.take {
                Take::Borrowed => r.try_slice::<u8>().is_some(),
                Take::Copied => r.try_bulk(&mut out)?,
                Take::Bytes => {
                    r.read_bytes()?;
                    true
                }
            };
            self.took.set(took);
            if took {
                Ok(())
            } else {
                beve::Read::read(&mut out, r)
            }
        }
    }

    let limit = beve::reader::MAX_DEPTH;
    for outer in [limit - 1, limit] {
        let mut doc: Vec<u8> = std::iter::repeat_n([header::GENERIC_ARRAY, 1 << 2], outer as usize)
            .flatten()
            .collect();
        doc.extend_from_slice(&to_beve(&vec![7u8]));
        for take in [Take::Borrowed, Take::Copied, Take::Bytes] {
            let took = &Cell::new(false);
            let read = beve::read_into(&mut Deep { outer, take, took }, &doc).map_err(|e| e.code);
            let fits = outer < limit;
            // A big-endian host never borrows a block or copies one whole,
            // whatever the width, so there both decline at every depth and the
            // fallback reads; what the depth decides is then only whether that
            // read succeeds. A byte slice has no byte order and is taken on
            // either.
            let can_take = matches!(take, Take::Bytes) || cfg!(target_endian = "little");
            assert_eq!(took.get(), fits && can_take, "{outer} containers");
            let refused = Err(ErrorCode::ExceededMaxDepth);
            assert_eq!(
                read,
                if fits { Ok(()) } else { refused },
                "{outer} containers"
            );
            assert_eq!(validate_beve(&doc).is_ok(), fits, "{outer} containers");
        }
    }
}

#[test]
fn a_value_this_crate_cannot_decode_still_validates() {
    // A 128-bit float is a width the specification defines and Rust has no
    // type for. Validation is about the bytes, not about what can hold them.
    let mut f128 = vec![header::number(header::CAT_FLOAT, 4)];
    f128.extend_from_slice(&[0u8; 16]);
    validate_beve(&f128).unwrap();
    assert!(from_beve::<f64>(&f128).is_err());
}

#[test]
fn the_error_carries_the_offset_the_walk_stopped_at() {
    let bytes = to_beve(&vec![1u32, 2, 3]);
    let err = validate_beve(&bytes[..bytes.len() - 1]).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnexpectedEnd);
    assert_eq!(err.index, 2);
}

// ---------------------------------------------------------------------------
// Through a reader
// ---------------------------------------------------------------------------

#[test]
fn validate_reader_drains_and_agrees_with_the_slice_form() {
    let bytes = to_beve(&everything());
    beve::validate_reader(&bytes[..]).unwrap();

    let short = &bytes[..bytes.len() / 2];
    let err = beve::validate_reader(short).unwrap_err();
    assert_eq!(
        err.as_parse().unwrap().code,
        validate_beve(short).unwrap_err().code
    );
}
