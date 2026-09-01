//! BEVE: the bytes on the wire, and what comes back off them.
//!
//! The golden vectors here were derived from the BEVE specification and then
//! checked byte for byte against an independent implementation, so they pin
//! interoperability rather than merely self-consistency.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use structio::beve::header;
use structio::{ErrorCode, SkipUnknown, beve, from_beve, from_beve_with, to_beve};

#[derive(Default, Debug, PartialEq, Clone)]
struct Sample {
    id: u32,
    name: String,
    values: Vec<f64>,
    ok: bool,
}
structio::object!(Sample {
    id,
    name,
    values,
    ok
});

#[derive(Default, Debug, PartialEq, Clone)]
struct Nested {
    inner: Sample,
    tags: Vec<String>,
    flags: Vec<bool>,
    blob: Vec<u8>,
}
structio::object!(Nested {
    inner,
    tags,
    flags,
    blob
});

fn sample() -> Sample {
    Sample {
        id: 1,
        name: "x".into(),
        values: vec![0.5],
        ok: true,
    }
}

fn nested() -> Nested {
    Nested {
        inner: Sample {
            id: 2,
            name: "y".into(),
            values: vec![],
            ok: false,
        },
        tags: vec!["t".into()],
        flags: vec![true; 9],
        blob: vec![9, 8],
    }
}

/// Round-trip through BEVE and confirm the value survived.
fn round<T>(value: &T) -> Vec<u8>
where
    T: beve::Write + for<'de> beve::Read<'de> + Default + PartialEq + std::fmt::Debug,
{
    let bytes = to_beve(value);
    let back: T = from_beve(&bytes).expect("round trip");
    assert_eq!(&back, value);
    bytes
}

// ---------------------------------------------------------------------------
// The bytes themselves
// ---------------------------------------------------------------------------

#[test]
fn scalars_match_the_specification() {
    assert_eq!(to_beve(&7u8), [0x11, 7]);
    assert_eq!(to_beve(&300u32), [0x51, 44, 1, 0, 0]);
    assert_eq!(
        to_beve(&-5i64),
        [0x69, 251, 255, 255, 255, 255, 255, 255, 255]
    );
    assert_eq!(to_beve(&1.5f64), [0x61, 0, 0, 0, 0, 0, 0, 248, 63]);
    assert_eq!(to_beve(&1.5f32), [0x41, 0, 0, 192, 63]);
    assert_eq!(to_beve(&true), [0x18]);
    assert_eq!(to_beve(&false), [0x08]);
    assert_eq!(to_beve(&Option::<u8>::None), [0x00]);
    assert_eq!(to_beve(&()), [0x00]);
    // A string is a header, a size, and its bytes; no terminator and no
    // escaping, so the length prefix is the whole story.
    assert_eq!(to_beve("hi"), [0x02, 8, b'h', b'i']);
}

#[test]
fn arrays_match_the_specification() {
    assert_eq!(
        to_beve(&vec![1.0f64, 2.0, 3.0]),
        [
            0x64, 12, // f64 typed array, three elements
            0, 0, 0, 0, 0, 0, 240, 63, //
            0, 0, 0, 0, 0, 0, 0, 64, //
            0, 0, 0, 0, 0, 0, 8, 64,
        ]
    );
    assert_eq!(to_beve(&vec![1u8, 2, 3]), [0x14, 12, 1, 2, 3]);
    // Booleans pack low bit first: 1, 0, 1 is 0b101.
    assert_eq!(to_beve(&vec![true, false, true]), [0x1C, 12, 0b101]);
    // String array elements carry a size but no header of their own.
    assert_eq!(
        to_beve(&vec!["a".to_string(), "bb".to_string()]),
        [0x3C, 8, 4, b'a', 8, b'b', b'b']
    );
    // A sequence with no typed form is a generic array of tagged values.
    assert_eq!(to_beve(&(1u8, "a")), [0x05, 8, 0x11, 1, 0x02, 4, b'a']);
}

#[test]
fn an_object_is_its_header_its_count_and_its_members() {
    assert_eq!(
        to_beve(&sample()),
        [
            0x03, 16, // object, four members
            8, b'i', b'd', 0x51, 1, 0, 0, 0, //
            16, b'n', b'a', b'm', b'e', 0x02, 4, b'x', //
            24, b'v', b'a', b'l', b'u', b'e', b's', 0x64, 4, 0, 0, 0, 0, 0, 0, 224, 63, //
            8, b'o', b'k', 0x18,
        ]
    );
}

#[test]
fn nesting_and_bit_packing_survive_a_round_trip() {
    let bytes = round(&nested());
    // Nine booleans: a full byte, then one bit of the next, with the padding
    // bits of the final byte zero.
    let flags = bytes.windows(2).position(|w| w == [0x1C, 36]).unwrap();
    assert_eq!(&bytes[flags..flags + 4], [0x1C, 36, 0xFF, 0x01]);
}

#[test]
fn an_integer_keyed_map_stores_integers() {
    let mut m = BTreeMap::new();
    m.insert(3u32, 4u32);
    // Object with unsigned 4-byte keys: header 3, category 2, width code 2.
    assert_eq!(to_beve(&m), [0x53, 4, 3, 0, 0, 0, 0x51, 4, 0, 0, 0]);
    round(&m);
}

#[test]
fn a_string_keyed_map_looks_like_an_object() {
    let mut m = BTreeMap::new();
    m.insert("k".to_string(), 1u8);
    assert_eq!(to_beve(&m), [0x03, 4, 4, b'k', 0x11, 1]);
    round(&m);
}

#[test]
fn the_size_codec_widens_at_its_documented_thresholds() {
    // 1 byte below 2^6, 2 below 2^14, 4 below 2^30, 8 beyond.
    assert_eq!(to_beve(&vec![0u8; 63])[1..2], [63 << 2]);
    assert_eq!(to_beve(&vec![0u8; 64])[1..3], [0b01, 1]);
    assert_eq!(to_beve(&vec![0u8; 16383])[1..3], [(63 << 2) | 0b01, 255]);
    assert_eq!(to_beve(&vec![0u8; 16384])[1..5], [0b10, 0, 1, 0]);

    // Every width, encoded and decoded back, including each boundary from
    // both sides.
    for n in [
        0u64,
        1,
        63,
        64,
        65,
        16383,
        16384,
        1 << 20,
        (1 << 30) - 1,
        1 << 30,
        u32::MAX as u64,
        header::MAX_SIZE,
    ] {
        let mut buf = [0u8; 8];
        let used = header::encode_size(n, &mut buf);
        assert_eq!(used, header::size_len(n), "width of {n}");
        let mut pos = 0;
        assert_eq!(header::decode_size(&buf[..used], &mut pos).unwrap(), n);
        assert_eq!(pos, used, "decoder consumed the wrong width for {n}");
    }
}

// ---------------------------------------------------------------------------
// Every supported type
// ---------------------------------------------------------------------------

#[test]
fn every_scalar_round_trips() {
    round(&u8::MAX);
    round(&u16::MAX);
    round(&u32::MAX);
    round(&u64::MAX);
    round(&usize::MAX);
    round(&u128::MAX);
    round(&i8::MIN);
    round(&i16::MIN);
    round(&i32::MIN);
    round(&i64::MIN);
    round(&isize::MIN);
    round(&i128::MIN);
    round(&i128::MAX);
    round(&f32::MIN);
    round(&f64::MAX);
    round(&true);
    round(&());
    round(&'\u{1F600}');
    round(&"héllo".to_string());
}

#[test]
fn non_finite_floats_survive_where_json_cannot_carry_them() {
    // BEVE stores the bits, so unlike JSON there is nothing to lose.
    let bytes = to_beve(&f64::INFINITY);
    assert_eq!(from_beve::<f64>(&bytes).unwrap(), f64::INFINITY);
    let bytes = to_beve(&f64::NAN);
    assert!(from_beve::<f64>(&bytes).unwrap().is_nan());
}

#[test]
fn every_container_round_trips() {
    round(&vec![1u32, 2, 3]);
    round(&vec![vec![1.0f32], vec![], vec![2.0, 3.0]]);
    round(&VecDeque::from(vec![1u8, 2]));
    round(&BTreeSet::from([1u16, 2, 3]));
    round(&[1u64, 2, 3]);
    round(&Some(4u8));
    round(&Option::<u8>::None);
    round(&Box::new(9u8));
    round(&(1u8, 2u16, "x".to_string()));
    round(&HashMap::from([("a".to_string(), 1u8)]));
    round(&BTreeMap::from([(-3i16, 'z')]));
    round(&sample());
    round(&nested());
    round(&vec![sample(), sample()]);
}

#[test]
fn a_deque_or_a_set_writes_a_generic_array_and_reads_back_either_way() {
    // Neither has one backing slice, so neither can be a typed array's
    // payload. The two forms stay interchangeable on the way in.
    let deque = VecDeque::from(vec![1.5f64, 2.5]);
    assert_eq!(to_beve(&deque)[0], header::GENERIC_ARRAY);
    let vec = vec![1.5f64, 2.5];
    assert_eq!(to_beve(&vec)[0], header::array_of(header::CAT_FLOAT, 3));

    assert_eq!(from_beve::<Vec<f64>>(&to_beve(&deque)).unwrap(), vec);
    assert_eq!(from_beve::<VecDeque<f64>>(&to_beve(&vec)).unwrap(), deque);
}

#[test]
fn an_empty_sequence_keeps_its_element_type() {
    // An empty typed array still names its elements, unlike a producer that
    // only learns the type from the first element it sees. Both forms read.
    assert_eq!(to_beve(&Vec::<f64>::new()), [0x64, 0]);
    assert_eq!(
        from_beve::<Vec<f64>>(&[0x64, 0]).unwrap(),
        Vec::<f64>::new()
    );
    assert_eq!(
        from_beve::<Vec<f64>>(&[header::GENERIC_ARRAY, 0]).unwrap(),
        Vec::<f64>::new()
    );
}

#[test]
fn bit_packing_is_right_at_every_boundary() {
    for n in 0..40usize {
        let flags: Vec<bool> = (0..n).map(|i| i % 3 == 0).collect();
        let bytes = round(&flags);
        let prefix = 1 + header::size_len(n as u64);
        assert_eq!(bytes.len(), prefix + n.div_ceil(8), "n = {n}");
        // The unused high bits of the final byte must be zero.
        if n % 8 != 0 {
            let last = *bytes.last().unwrap();
            assert_eq!(last >> (n % 8), 0, "padding bits set for n = {n}");
        }
    }
}

// ---------------------------------------------------------------------------
// Borrowing
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Borrowed<'a> {
    name: &'a str,
    blob: &'a [u8],
}
structio::beve_object!(['de] Borrowed<'de> { name, blob });

#[test]
fn strings_and_byte_arrays_borrow_out_of_the_input() {
    let owned = Borrowed {
        name: "no copy",
        blob: &[1, 2, 3, 4],
    };
    let bytes = to_beve(&owned);
    let back: Borrowed = from_beve(&bytes).unwrap();
    assert_eq!(back, owned);
    // Really pointing into the buffer, not at a copy.
    let base = bytes.as_ptr() as usize;
    let at = back.name.as_ptr() as usize;
    assert!(at >= base && at < base + bytes.len());
}

#[test]
fn a_borrowed_byte_array_rejects_wider_elements() {
    // A run of `u32` is not a run of bytes, whatever its length in bytes.
    let bytes = to_beve(&vec![1u32, 2]);
    let e = from_beve::<&[u8]>(&bytes).unwrap_err();
    assert_eq!(e.code, ErrorCode::ExpectedBytes);
}

// ---------------------------------------------------------------------------
// Leniency about width, strictness about kind
// ---------------------------------------------------------------------------

#[test]
fn an_integer_field_takes_any_width_that_fits() {
    // What a `u8` producer wrote, read into every wider target.
    let narrow = to_beve(&200u8);
    assert_eq!(from_beve::<u64>(&narrow).unwrap(), 200);
    assert_eq!(from_beve::<i32>(&narrow).unwrap(), 200);
    assert_eq!(from_beve::<u128>(&narrow).unwrap(), 200);
    assert_eq!(from_beve::<f64>(&narrow).unwrap(), 200.0);

    // And what does not fit is reported rather than truncated.
    let wide = to_beve(&300u32);
    assert_eq!(
        from_beve::<u8>(&wide).unwrap_err().code,
        ErrorCode::NumberOutOfRange
    );
    let negative = to_beve(&-1i8);
    assert_eq!(
        from_beve::<u32>(&negative).unwrap_err().code,
        ErrorCode::NumberOutOfRange
    );
}

#[test]
fn a_typed_array_of_one_width_reads_into_a_vec_of_another() {
    let stored = to_beve(&vec![1u8, 2, 3]);
    assert_eq!(from_beve::<Vec<u64>>(&stored).unwrap(), vec![1u64, 2, 3]);
    assert_eq!(from_beve::<Vec<f64>>(&stored).unwrap(), vec![1.0, 2.0, 3.0]);
    // The bulk path only fires on an exact match; the widening path has to
    // produce the same answer.
    assert_eq!(from_beve::<Vec<u8>>(&stored).unwrap(), vec![1u8, 2, 3]);
}

#[test]
fn a_float_field_takes_the_half_widths_it_will_never_write() {
    // bfloat16 is the top half of an f32.
    let bf16 = [header::number(header::CAT_FLOAT, 0), 0x80, 0x3F];
    assert_eq!(from_beve::<f64>(&bf16).unwrap(), 1.0);
    // IEEE binary16: 1.0 is 0x3C00, and the smallest subnormal is 2^-24.
    let f16 = [header::number(header::CAT_FLOAT, 1), 0x00, 0x3C];
    assert_eq!(from_beve::<f64>(&f16).unwrap(), 1.0);
    // The smallest binary16 subnormal, 2^-24, written as an exact quotient
    // rather than through `powi`, which is not required to be exact.
    let sub = [header::number(header::CAT_FLOAT, 1), 0x01, 0x00];
    assert_eq!(from_beve::<f64>(&sub).unwrap(), 1.0 / 16_777_216.0);
    let neg = [header::number(header::CAT_FLOAT, 1), 0x00, 0xBC];
    assert_eq!(from_beve::<f64>(&neg).unwrap(), -1.0);
    let inf = [header::number(header::CAT_FLOAT, 1), 0x00, 0x7C];
    assert!(from_beve::<f64>(&inf).unwrap().is_infinite());
}

#[test]
fn a_128_bit_float_is_reported_rather_than_guessed_at() {
    let mut bytes = vec![header::number(header::CAT_FLOAT, 4)];
    bytes.extend_from_slice(&[0u8; 16]);
    assert_eq!(
        from_beve::<f64>(&bytes).unwrap_err().code,
        ErrorCode::UnsupportedFeature
    );
}

#[test]
fn the_wrong_kind_is_an_error_and_never_a_conversion() {
    let text = to_beve("5");
    assert_eq!(
        from_beve::<u32>(&text).unwrap_err().code,
        ErrorCode::ExpectedNumber
    );
    let number = to_beve(&5u32);
    assert_eq!(
        from_beve::<String>(&number).unwrap_err().code,
        ErrorCode::ExpectedString
    );
    assert_eq!(
        from_beve::<bool>(&number).unwrap_err().code,
        ErrorCode::ExpectedBool
    );
    assert_eq!(
        from_beve::<Vec<u8>>(&number).unwrap_err().code,
        ErrorCode::ExpectedArray
    );
    assert_eq!(
        from_beve::<Sample>(&number).unwrap_err().code,
        ErrorCode::ExpectedObject
    );
    // A float is not an integer even when its value is integral.
    let float = to_beve(&5.0f64);
    assert_eq!(
        from_beve::<u32>(&float).unwrap_err().code,
        ErrorCode::ExpectedInteger
    );
}

#[test]
fn an_element_of_the_wrong_kind_reports_itself_the_same_way() {
    // Inside a typed array the element header is implied, and the reader that
    // meets it should not be able to tell the difference.
    let bools = to_beve(&vec![true, false]);
    assert_eq!(
        from_beve::<Vec<String>>(&bools).unwrap_err().code,
        ErrorCode::ExpectedString
    );
    let strings = to_beve(&vec!["a".to_string()]);
    assert_eq!(
        from_beve::<Vec<u32>>(&strings).unwrap_err().code,
        ErrorCode::ExpectedNumber
    );
}

/// An implied header is a header, and the paths that look ahead for a block
/// worth taking whole have to see it as one. A sequence cannot occur inside a
/// typed array, so what they would otherwise look at is the element's payload:
/// a `f64` whose bytes begin with an array header would be read as an array of
/// nothing rather than as the number it is.
#[test]
fn a_payload_byte_is_never_mistaken_for_the_header_a_typed_array_implied() {
    let trap = f64::from_le_bytes([header::array_of(header::CAT_FLOAT, 3), 0, 0, 0, 0, 0, 0, 0]);
    let doc = to_beve(&vec![trap, 1.0]);
    let mut docs = beve::Documents::array(&doc[..]);
    // Each element is a number, so asking for a sequence is the ordinary
    // mismatch, at both of them.
    for value in docs.iter::<Vec<f64>>() {
        let failure = value.unwrap_err();
        assert_eq!(
            failure.as_parse().map(|e| e.code),
            Some(ErrorCode::ExpectedArray),
            "{failure}"
        );
    }
}

#[test]
fn a_struct_will_not_read_an_integer_keyed_object() {
    let mut m = BTreeMap::new();
    m.insert(1u8, 1u32);
    assert_eq!(
        from_beve::<Sample>(&to_beve(&m)).unwrap_err().code,
        ErrorCode::UnsupportedKeyType
    );
}

// ---------------------------------------------------------------------------
// Unknown members
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Two {
    a: u32,
    b: u32,
}
structio::object!(Two { a, b });

/// Assemble an object out of `(key, value bytes)` pairs.
fn object(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![header::OBJECT];
    let mut size = [0u8; 8];
    let used = header::encode_size(members.len() as u64, &mut size);
    out.extend_from_slice(&size[..used]);
    for (key, value) in members {
        let used = header::encode_size(key.len() as u64, &mut size);
        out.extend_from_slice(&size[..used]);
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(value);
    }
    out
}

#[test]
fn an_unknown_member_is_stepped_over_whatever_it_holds() {
    let extras: Vec<(&str, Vec<u8>)> = vec![
        ("z_null", to_beve(&())),
        ("z_bool", to_beve(&true)),
        ("z_num", to_beve(&1.5f64)),
        ("z_str", to_beve("skip me")),
        ("z_obj", to_beve(&nested())),
        ("z_arr", to_beve(&vec![1u8, 2, 3])),
        ("z_bools", to_beve(&vec![true; 9])),
        ("z_strs", to_beve(&vec!["a".to_string(), "b".to_string()])),
        ("z_generic", to_beve(&(1u8, 2u16))),
        ("z_map", to_beve(&BTreeMap::from([(1u8, 2u8)]))),
    ];
    for (name, value) in &extras {
        let doc = object(&[
            ("a", to_beve(&1u32)),
            (name, value.clone()),
            ("b", to_beve(&2u32)),
        ]);
        assert_eq!(
            from_beve_with::<SkipUnknown, Two>(&doc).unwrap(),
            Two { a: 1, b: 2 },
            "skipping {name}"
        );
    }
}

#[test]
fn an_unknown_member_holding_an_extension_is_stepped_over_too() {
    // None of these decode into a Rust type, but all of them state their own
    // extent, so a document carrying one stays readable for its other fields.
    let delimiter = vec![header::header(header::TY_EXTENSION, 0, 0)];

    // A matrix: layout byte, extents as a typed array, data as a typed array.
    let mut matrix = vec![header::header(header::TY_EXTENSION, 0, 0) | (header::EXT_MATRIX << 3)];
    matrix.push(0); // layout_right
    matrix.extend_from_slice(&to_beve(&vec![1u32, 2]));
    matrix.extend_from_slice(&to_beve(&vec![1.0f64, 2.0]));

    // One complex f64: the extension header, a numeric header, and two values.
    let mut complex = vec![header::header(header::TY_EXTENSION, 0, 0) | (header::EXT_COMPLEX << 3)];
    complex.push(header::header(0, header::CAT_FLOAT, 3));
    complex.extend_from_slice(&1.0f64.to_le_bytes());
    complex.extend_from_slice(&2.0f64.to_le_bytes());

    // The deprecated type tag: an index and the value it tagged.
    let mut tag = vec![header::header(header::TY_EXTENSION, 0, 0) | (header::EXT_TYPE_TAG << 3)];
    tag.push(0); // index 0
    tag.extend_from_slice(&to_beve(&7u8));

    for (name, value) in [
        ("delimiter", delimiter),
        ("matrix", matrix),
        ("complex", complex),
        ("tag", tag),
    ] {
        let doc = object(&[("a", to_beve(&1u32)), ("z", value), ("b", to_beve(&2u32))]);
        assert_eq!(
            from_beve_with::<SkipUnknown, Two>(&doc).unwrap(),
            Two { a: 1, b: 2 },
            "skipping {name}"
        );
    }
}

/// An aligned typed array, which nothing in this crate writes.
///
/// `HEADER | NUMERIC_HEADER | SIZE | PADDING_LENGTH | PADDING | DATA`, where the
/// padding exists so a reader can point at `DATA` directly. The writer here
/// never emits this form, so these vectors stand in for a producer that does.
fn aligned(numeric_header: u8, count: usize, pad: usize, data: &[u8]) -> Vec<u8> {
    let mut v = vec![header::ALIGNED_ARRAY, numeric_header];
    v.push((count as u8) << 2); // a compressed size, one byte up to 63
    v.push(pad as u8);
    v.extend(std::iter::repeat_n(0xAA, pad));
    v.extend_from_slice(data);
    v
}

#[test]
fn an_aligned_array_is_read_and_skipped_at_its_true_extent() {
    // Three paths handle this form: reading it as bytes, driving it as numbers,
    // and stepping over it. All three must agree on where it ends, or a field
    // after one is parsed from inside its payload.
    let mut f64s = Vec::new();
    for v in [1.0f64, 2.0, 3.0] {
        f64s.extend_from_slice(&v.to_le_bytes());
    }
    let floats = aligned(header::array_of(header::CAT_FLOAT, 3), 3, 7, &f64s);
    assert_eq!(from_beve::<Vec<f64>>(&floats).unwrap(), vec![1.0, 2.0, 3.0]);

    // The same payload widens element by element, as a typed array does.
    let bytes = aligned(header::array_of(header::CAT_UNSIGNED, 0), 3, 1, &[7, 8, 9]);
    assert_eq!(from_beve::<Vec<u64>>(&bytes).unwrap(), vec![7, 8, 9]);

    // Read as bytes, it borrows out of the input past the padding.
    assert_eq!(from_beve::<&[u8]>(&bytes).unwrap(), &[7u8, 8, 9]);

    // And an unknown member holding one is stepped over exactly.
    for (name, value) in [("floats", floats), ("bytes", bytes)] {
        let doc = object(&[("a", to_beve(&1u32)), ("z", value), ("b", to_beve(&2u32))]);
        assert_eq!(
            from_beve_with::<SkipUnknown, Two>(&doc).unwrap(),
            Two { a: 1, b: 2 },
            "skipping an aligned array of {name}"
        );
    }

    // Zero padding is the ordinary case once a producer is already aligned.
    let none = aligned(header::array_of(header::CAT_UNSIGNED, 0), 2, 0, &[4, 5]);
    assert_eq!(from_beve::<Vec<u8>>(&none).unwrap(), vec![4, 5]);

    // A truncated payload is refused rather than read short.
    let mut short = aligned(header::array_of(header::CAT_FLOAT, 3), 3, 7, &f64s);
    short.truncate(short.len() - 1);
    assert!(from_beve::<Vec<f64>>(&short).is_err());
}

#[test]
fn the_complex_headers_array_flag_is_three_bits_wide() {
    // The specification gives the flag three bits so that the class and byte
    // count land where a number header puts them, but defines only two values:
    //
    //     0 -> complex number    HEADER | COMPLEX HEADER | DATA
    //     1 -> complex array     HEADER | COMPLEX HEADER | SIZE | DATA
    //
    // The two differ by the SIZE in front of the payload, so reading the flag
    // wrongly does not fail loudly: it consumes the wrong number of bytes and
    // the *next* member is parsed from inside this one. These vectors pin the
    // widths, and the golden bytes are written out rather than computed so that
    // a change to the header helpers cannot move the goalposts with the code.
    let ext = header::header(header::TY_EXTENSION, 0, 0) | (header::EXT_COMPLEX << 3);
    assert_eq!(ext, 0x1E);

    // One complex u16. The complex header packs the flag into bits 0-2, the
    // class into 3-4 and the byte count into 5-7, which is where a number
    // header puts the last two: 0b001_10_000, byte count 1, class 2
    // (unsigned), flag 0.
    const NUMBER: u8 = 0x30;
    const ARRAY: u8 = 0x31; // the same, flag 1
    let single = vec![0x1E, NUMBER, 1, 2, 3, 4];
    // A run of one: a compressed size in front of the payload.
    let array = vec![0x1E, ARRAY, 0x04, 1, 2, 3, 4];

    for (name, value) in [("single", single), ("array", array)] {
        let doc = object(&[("a", to_beve(&1u32)), ("z", value), ("b", to_beve(&2u32))]);
        assert_eq!(
            from_beve_with::<SkipUnknown, Two>(&doc).unwrap(),
            Two { a: 1, b: 2 },
            "skipping a complex {name}"
        );
    }

    // Every other value of the field is unspecified, and the specification
    // requires unspecified bits to be zero. Guessing would make the extent of
    // the value depend on bits that carry no meaning, so these are refused.
    // Note that the first payload byte here is a valid compressed size, so a
    // reader that treated these as arrays would accept them and silently
    // consume the wrong extent rather than erroring.
    for flag in 2u8..8 {
        let value = vec![0x1E, NUMBER | flag, 0x04, 1, 2, 3, 4];
        let doc = object(&[("a", to_beve(&1u32)), ("z", value), ("b", to_beve(&2u32))]);
        assert_eq!(
            from_beve_with::<SkipUnknown, Two>(&doc).unwrap_err().code,
            ErrorCode::InvalidHeader,
            "complex flag {flag} is not defined and must not be guessed at"
        );
    }
}

#[test]
fn a_missing_member_leaves_the_field_as_it_was() {
    let doc = object(&[("b", to_beve(&9u32))]);
    let mut value = Two { a: 5, b: 0 };
    beve::read_into(&mut value, &doc).unwrap();
    assert_eq!(value, Two { a: 5, b: 9 });
}

#[test]
fn a_key_that_collides_with_a_bucket_is_still_rejected() {
    // The perfect hash only proposes a field; the full comparison is what
    // decides. A key of the right length that hashes into an occupied bucket
    // must be treated as unknown, not as the field it landed on.
    for key in ["A", "aa", "ab", "\u{00e1}", "z"] {
        let doc = object(&[(key, to_beve(&7u32)), ("b", to_beve(&2u32))]);
        let got = from_beve_with::<SkipUnknown, Two>(&doc).unwrap();
        assert_eq!(got, Two { a: 0, b: 2 }, "key {key:?}");
    }
}

// ---------------------------------------------------------------------------
// One schema, two formats
// ---------------------------------------------------------------------------

#[test]
fn the_same_struct_round_trips_through_either_format() {
    let value = nested();
    let text = structio::to_string(&value);
    let binary = to_beve(&value);
    assert_eq!(structio::from_str::<Nested>(&text).unwrap(), value);
    assert_eq!(from_beve::<Nested>(&binary).unwrap(), value);
    // And the binary is smaller than the text, which is the point.
    assert!(
        binary.len() < text.len(),
        "{} vs {}",
        binary.len(),
        text.len()
    );
}

#[derive(Default, Debug, PartialEq)]
struct Renamed {
    first_name: String,
}
structio::object!(Renamed { "first-name" => first_name });

#[test]
fn a_renamed_key_is_renamed_in_both_formats() {
    let value = Renamed {
        first_name: "ada".into(),
    };
    assert_eq!(structio::to_string(&value), r#"{"first-name":"ada"}"#);
    let bytes = to_beve(&value);
    assert!(
        bytes.windows(10).any(|w| w == b"first-name"),
        "key not found in {bytes:?}"
    );
    assert_eq!(from_beve::<Renamed>(&bytes).unwrap(), value);
}

#[derive(Default, Debug, PartialEq)]
struct Page<T> {
    items: Vec<T>,
    cursor: Option<String>,
}
structio::object!([T: structio::ReadWrite + Default] Page<T> { items, cursor });

#[test]
fn a_generic_struct_works_in_both_formats() {
    let page = Page {
        items: vec![1u32, 2, 3],
        cursor: Some("next".into()),
    };
    assert_eq!(
        structio::from_str::<Page<u32>>(&structio::to_string(&page)).unwrap(),
        page
    );
    round(&page);
}

// ---------------------------------------------------------------------------
// Reuse
// ---------------------------------------------------------------------------

#[test]
fn reading_into_a_used_value_reuses_its_buffers() {
    let mut value = Sample::default();
    let bytes = to_beve(&sample());
    beve::read_into(&mut value, &bytes).unwrap();
    let name_at = value.name.as_ptr();
    let values_at = value.values.as_ptr();
    beve::read_into(&mut value, &bytes).unwrap();
    assert_eq!(value, sample());
    assert_eq!(value.name.as_ptr(), name_at);
    assert_eq!(value.values.as_ptr(), values_at);
}

#[test]
fn a_shorter_array_truncates_rather_than_appending() {
    let mut value: Vec<u32> = vec![9, 9, 9, 9, 9];
    beve::read_into(&mut value, &to_beve(&vec![1u32, 2])).unwrap();
    assert_eq!(value, vec![1, 2]);
    // And the widening path, which does not go through the bulk copy.
    let mut value: Vec<u32> = vec![9, 9, 9, 9, 9];
    beve::read_into(&mut value, &to_beve(&vec![1u8, 2])).unwrap();
    assert_eq!(value, vec![1, 2]);
}

#[test]
fn writing_into_an_existing_buffer_keeps_its_allocation() {
    let mut buf = Vec::with_capacity(1024);
    let at = buf.as_ptr();
    beve::write_into(&sample(), &mut buf);
    assert_eq!(buf, to_beve(&sample()));
    assert_eq!(buf.as_ptr(), at);
}

// ---------------------------------------------------------------------------
// Malformed input
// ---------------------------------------------------------------------------

#[test]
fn every_truncation_of_a_document_is_reported() {
    let full = to_beve(&nested());
    for cut in 0..full.len() {
        let e = from_beve::<Nested>(&full[..cut]).unwrap_err();
        assert!(
            matches!(
                e.code,
                ErrorCode::UnexpectedEnd | ErrorCode::TrailingContent
            ),
            "cut {cut} gave {:?}",
            e.code
        );
    }
    assert!(from_beve::<Nested>(&full).is_ok());
}

#[test]
fn trailing_bytes_are_not_ignored() {
    let mut bytes = to_beve(&7u8);
    bytes.push(0);
    assert_eq!(
        from_beve::<u8>(&bytes).unwrap_err().code,
        ErrorCode::TrailingContent
    );
}

#[test]
fn a_count_larger_than_the_document_cannot_make_it_allocate() {
    // Four billion elements, and four bytes of payload.
    let mut bytes = vec![header::array_of(header::CAT_FLOAT, 3)];
    bytes.extend_from_slice(&[0b10, 255, 255, 255]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    assert_eq!(
        from_beve::<Vec<f64>>(&bytes).unwrap_err().code,
        ErrorCode::UnexpectedEnd
    );
    // The generic form has to refuse just as quickly, one element at a time.
    let mut bytes = vec![header::GENERIC_ARRAY];
    bytes.extend_from_slice(&[0b10, 255, 255, 255]);
    assert_eq!(
        from_beve::<Vec<f64>>(&bytes).unwrap_err().code,
        ErrorCode::UnexpectedEnd
    );
}

#[test]
fn deep_nesting_is_refused_before_the_stack_runs_out() {
    // Reading is bounded by the destination type, which cannot nest without
    // limit, so the guard exists for the one path that recurses on the
    // document's shape instead: stepping over a member nothing claimed.
    let mut bytes = Vec::new();
    for _ in 0..2000 {
        bytes.push(header::GENERIC_ARRAY);
        bytes.push(1 << 2); // one element
    }
    bytes.push(header::NULL);
    let doc = object(&[("z", bytes), ("a", to_beve(&1u32))]);
    assert_eq!(
        from_beve_with::<SkipUnknown, Two>(&doc).unwrap_err().code,
        ErrorCode::ExceededMaxDepth
    );
}

#[test]
fn a_string_that_is_not_utf8_is_refused() {
    let bytes = [header::STRING, 2 << 2, 0xFF, 0xFE];
    assert_eq!(
        from_beve::<String>(&bytes).unwrap_err().code,
        ErrorCode::InvalidUtf8
    );
}

#[test]
fn a_reserved_type_is_refused_rather_than_guessed_at() {
    // Type 7 is reserved by the specification and means nothing yet.
    assert!(from_beve::<u8>(&[0b111]).is_err());
    let doc = object(&[("z", vec![0b111]), ("a", to_beve(&1u32))]);
    assert_eq!(
        from_beve_with::<SkipUnknown, Two>(&doc).unwrap_err().code,
        ErrorCode::InvalidHeader
    );
}

#[test]
fn an_error_is_located_where_it_was_found() {
    let doc = object(&[("a", to_beve(&1u32)), ("b", to_beve("not a number"))]);
    let e = from_beve::<Two>(&doc).unwrap_err();
    assert_eq!(e.code, ErrorCode::ExpectedNumber);
    // The `b` member's value begins after the header, count, both keys, and
    // the first value.
    assert_eq!(doc[e.index - 1], header::STRING);
}

// ---------------------------------------------------------------------------
// Writing through a sink
// ---------------------------------------------------------------------------

#[test]
fn sink_output_matches_the_in_memory_writer_at_every_buffer_size() {
    let value = nested();
    let want = to_beve(&value);
    for cap in 1..=want.len() + 4 {
        let mut got = Vec::new();
        beve::to_writer_buffered(&value, &mut got, cap).unwrap();
        assert_eq!(got, want, "buffer of {cap}");
    }
}

#[test]
fn a_failing_sink_is_reported_once_and_stops_growing() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("no"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let big = vec![0u8; 100_000];
    let e = beve::to_writer_buffered(&big, Broken, 16).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::Other);
}

#[test]
fn from_reader_reads_what_to_writer_wrote() {
    let value = nested();
    let mut bytes = Vec::new();
    beve::to_writer(&value, &mut bytes).unwrap();
    let back: Nested = beve::from_reader(std::io::Cursor::new(&bytes)).unwrap();
    assert_eq!(back, value);
}
