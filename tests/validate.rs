//! Checking a BEVE document without decoding it.
//!
//! Validation walks the same headers reading does, so what it must never do is
//! disagree with reading about where a value ends: a document that validates
//! and then fails to read for a structural reason, or the reverse, would mean
//! two different notions of the format. Most of what is here is that
//! agreement, checked over every construct the writer can emit and over
//! corruption of each one.

use std::collections::HashMap;

use structio::beve::header;
use structio::{ErrorCode, SkipUnknown, beve, from_beve, from_beve_with, to_beve, validate_beve};

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
fn the_four_defined_extensions_validate() {
    let ext = |id: u8| header::header(header::TY_EXTENSION, 0, 0) | (id << 3);
    let bytes = |v: [u8; 2]| {
        vec![
            header::array_of(header::CAT_UNSIGNED, 0),
            2 << 2,
            v[0],
            v[1],
        ]
    };

    // A delimiter is a marker with no body.
    validate_beve(&[ext(header::EXT_DELIMITER)]).unwrap();

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
    }
    for h in [header::NULL, header::FALSE, header::TRUE] {
        validate_beve(&[h]).unwrap();
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
