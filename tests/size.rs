//! `beve::size` against the bytes it claims to predict.
//!
//! The contract is a single equation, `size_with::<P, _>(v) ==
//! to_vec_with::<P, _>(v).len()`, and it has to hold for every value and every
//! policy or it is worth nothing: a frame header carrying a length that is one
//! byte out is a stream the receiver cannot resynchronize.
//!
//! It holds in the same form for a value that does not begin the document:
//! `size_after(v, n)` is what `append` adds to a buffer of length `n`. That is the
//! aligned form's real case, its padding being chosen from where each payload
//! lands, so a body measured at zero and written behind a header is measured
//! wrongly by up to fifteen bytes an array.
//!
//! Measuring reuses the writer rather than describing the format a second
//! time, so most of what could go wrong is not expressible. What is left is
//! the handful of places the writer appends without going through `push` or
//! `raw` -- numbers, complex numbers, compressed sizes, and the aligned form's
//! padding -- and those are what the corpus below is chosen to reach.
//!
//! One thing this does *not* check, despite appearances. Every built-in policy
//! is measured, but `SKIP_NULL` is the only constant the BEVE writer reads, so
//! the others produce byte-identical binary and cannot tell a forwarded
//! constant in `Measured` from a dropped one: delete six of the seven and this
//! file still passes. They are kept as a standing guard for the day a new
//! constant does reach the writer, not as a live check that the forwarding is
//! complete. `src/options.rs` says what actually holds that promise.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::net::Ipv4Addr;
use std::time::Duration;

use structio::{
    AllowComments, Complex, ErrorCode, Matrix, MatrixLayout, Options, Pretty, PrettyInlineArrays,
    RequireKeys, SkipNull, SkipUnknown, Standard, beve,
};

// ---------------------------------------------------------------------------
// The check
// ---------------------------------------------------------------------------

/// A policy of no built-in shape, so the forwarding is exercised by something
/// other than the crate's own list.
#[derive(Clone, Copy)]
struct Custom;

impl Options for Custom {
    const PRETTY: bool = true;
    const INDENT: usize = 7;
    const NEW_LINES_IN_ARRAYS: bool = false;
    const SKIP_NULL: bool = true;
    const ERROR_ON_UNKNOWN_KEYS: bool = false;
    const ERROR_ON_MISSING_KEYS: bool = true;
    const ALLOW_COMMENTS: bool = true;
}

/// Assert the equation under one policy, both forms.
fn check_policy<O: Options, T: beve::Write + ?Sized>(what: &str, value: &T) {
    assert_eq!(
        beve::size_with::<O, T>(value),
        beve::to_vec_with::<O, T>(value).len(),
        "{what}: measured the plain form wrongly under {}",
        std::any::type_name::<O>(),
    );
    assert_eq!(
        beve::size_aligned_with::<O, T>(value),
        beve::to_vec_aligned_with::<O, T>(value).len(),
        "{what}: measured the aligned form wrongly under {}",
        std::any::type_name::<O>(),
    );
}

/// The same equation for a value that does not begin the document: what the
/// measurement claims is what appending to a buffer of that length adds.
///
/// The aligned form is the one that moves, its padding being chosen from where
/// each payload lands rather than from where the value starts, so measuring at
/// zero and appending behind a prefix would disagree. Both forms are checked
/// all the same, since a hand-written implementation can position itself in
/// either.
fn check_after<O: Options, T: beve::Write + ?Sized>(what: &str, value: &T, prefix_len: usize) {
    let prefix = vec![0xAA; prefix_len];
    let under = std::any::type_name::<O>();

    let mut out = prefix.clone();
    beve::append_with::<O, T>(value, &mut out);
    assert_eq!(
        beve::size_after_with::<O, T>(value, prefix_len),
        out.len() - prefix_len,
        "{what}: measured the plain form wrongly behind {prefix_len} bytes under {under}",
    );
    assert_eq!(
        &out[..prefix_len],
        &prefix[..],
        "{what}: appending overwrote the prefix"
    );

    let mut out = prefix.clone();
    beve::append_aligned_with::<O, T>(value, &mut out);
    assert_eq!(
        beve::size_aligned_after_with::<O, T>(value, prefix_len),
        out.len() - prefix_len,
        "{what}: measured the aligned form wrongly behind {prefix_len} bytes under {under}",
    );
    assert_eq!(
        &out[..prefix_len],
        &prefix[..],
        "{what}: appending overwrote the prefix"
    );
}

/// Assert the equation under every policy this crate ships, plus one it does
/// not, and check that the un-suffixed entry points agree with `Standard`.
fn check<T: beve::Write + ?Sized>(what: &str, value: &T) {
    check_policy::<Standard, T>(what, value);
    check_policy::<Pretty, T>(what, value);
    check_policy::<PrettyInlineArrays, T>(what, value);
    check_policy::<SkipNull, T>(what, value);
    check_policy::<SkipUnknown, T>(what, value);
    check_policy::<RequireKeys, T>(what, value);
    check_policy::<AllowComments, T>(what, value);
    check_policy::<Custom, T>(what, value);

    assert_eq!(beve::size(value), beve::to_vec(value).len(), "{what}");
    assert_eq!(
        beve::size_aligned(value),
        beve::to_vec_aligned(value).len(),
        "{what}"
    );

    // Appending to an empty buffer is writing from the beginning, in both
    // forms, or the offset-aware entry points are measuring some other
    // document than the ones above.
    let mut plain = Vec::new();
    beve::append(value, &mut plain);
    assert_eq!(plain, beve::to_vec(value), "{what}");
    let mut aligned = Vec::new();
    beve::append_aligned(value, &mut aligned);
    assert_eq!(aligned, beve::to_vec_aligned(value), "{what}");

    // Two offsets rather than a sweep: what actually depends on the offset is
    // the aligned form's padding, and `aligned_padding_at_every_base` walks
    // that exhaustively. These are here so that every value in the corpus is
    // measured somewhere other than zero, and under the one policy the BEVE
    // writer actually reads as well as under the default.
    check_after::<Standard, T>(what, value, 1);
    check_after::<SkipNull, T>(what, value, 13);
}

// ---------------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------------

#[test]
fn scalars() {
    check("unit", &());
    check("true", &true);
    check("false", &false);
    check("char", &'ß');

    check("u8", &7u8);
    check("u16", &7u16);
    check("u32", &7u32);
    check("u64", &u64::MAX);
    check("u128", &u128::MAX);
    check("usize", &usize::MAX);
    check("i8", &-7i8);
    check("i16", &i16::MIN);
    check("i32", &i32::MIN);
    check("i64", &i64::MIN);
    check("i128", &i128::MIN);
    check("isize", &isize::MIN);

    check("f32", &1.5f32);
    check("f64", &f64::NAN);
    check("-0.0", &-0.0f64);
    check("inf", &f64::INFINITY);
}

#[test]
fn strings() {
    check("empty", &String::new());
    check("str", "the quick brown fox");
    check("cow borrowed", &Cow::Borrowed("borrowed"));
    check("cow owned", &Cow::<str>::Owned("owned".into()));
    check("multibyte", &"ßßß✓✓✓".to_string());
}

#[test]
fn wrappers() {
    check("some", &Some(3u8));
    check("none", &Option::<u8>::None);
    check("nested none", &Some(Option::<u8>::None));
    check("box", &Box::new(3u8));
    check("rc", &std::rc::Rc::new("x".to_string()));
    check("arc", &std::sync::Arc::new(vec![1u16, 2, 3]));
}

// ---------------------------------------------------------------------------
// Sequences
// ---------------------------------------------------------------------------

#[test]
fn typed_arrays() {
    // Every element width, which is what the aligned form's padding turns on.
    check("u8s", &vec![1u8, 2, 3]);
    check("u16s", &vec![1u16, 2, 3]);
    check("u32s", &vec![1u32, 2, 3]);
    check("u64s", &vec![1u64, 2, 3]);
    check("u128s", &vec![1u128, 2, 3]);
    check("i8s", &vec![-1i8, 2, 3]);
    check("f32s", &vec![1.5f32, 2.5]);
    check("f64s", &vec![1.5f64, 2.5, 3.5]);

    check("empty f64s", &Vec::<f64>::new());
    check("slice", &[1.5f64, 2.5][..]);
    check("fixed array", &[1.5f64, 2.5, 3.5]);
}

#[test]
fn packed_booleans() {
    // The tail byte is the case: a length that is not a multiple of eight
    // writes a byte the loop's own condition does not reach.
    for n in 0..40usize {
        check("bools", &(0..n).map(|i| i % 3 == 0).collect::<Vec<bool>>());
    }
}

#[test]
fn string_arrays() {
    check(
        "strings",
        &vec!["a".to_string(), "bb".into(), String::new()],
    );
    check("empty strings", &Vec::<String>::new());
    check("chars", &vec!['a', 'ß', '✓']);
}

#[test]
fn generic_arrays() {
    check("options", &vec![Some(1u8), None, Some(3)]);
    check("nested", &vec![vec![1u8, 2], vec![], vec![3]]);
    check("tuple", &(1u8, "two".to_string(), 3.0f64));
    check("deque", &VecDeque::from([1u32, 2, 3]));
    check("btreeset", &BTreeSet::from([1u32, 2, 3]));
    check("hashset", &HashSet::from([1u32]));
}

// ---------------------------------------------------------------------------
// Maps
// ---------------------------------------------------------------------------

#[test]
fn maps() {
    check(
        "string keys",
        &BTreeMap::from([("a".to_string(), 1u8), ("bb".into(), 2)]),
    );
    check("integer keys", &BTreeMap::from([(1u16, -20i32), (2, 40)]));
    check("signed keys", &BTreeMap::from([(-1i64, 1u8)]));
    check(
        "hash map",
        &HashMap::from([("k".to_string(), vec![1.5f64])]),
    );
    check("empty map", &BTreeMap::<String, u8>::new());
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Reading {
    sensor: String,
    samples: Vec<f64>,
    valid: Vec<bool>,
    note: Option<String>,
}
structio::object!(Reading {
    sensor,
    samples,
    valid,
    note
});

/// A renamed key, whose bytes the macro pre-encodes: the length prefix in
/// front of it is part of the constant, so measuring it is `raw`'s business
/// rather than `size`'s.
#[derive(Default)]
struct Outer {
    head: Reading,
    tail: Vec<Reading>,
    flag: bool,
}
structio::object!(Outer {
    head,
    "a rather longer key than the field name" => tail,
    flag
});

#[derive(Default)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}
structio::array!(Vec3 [f64; x, y, z]);

#[derive(Default)]
struct Mixed {
    name: String,
    count: u32,
}
structio::array!(Mixed [name, count]);

fn reading(sensor: &str, n: usize) -> Reading {
    Reading {
        sensor: sensor.into(),
        samples: (0..n).map(|i| i as f64).collect(),
        valid: (0..n).map(|i| i % 2 == 0).collect(),
        note: if n == 0 { None } else { Some("note".into()) },
    }
}

#[test]
fn structs() {
    check("empty struct", &Reading::default());
    check("struct", &reading("thermocouple", 9));
    check(
        "array struct",
        &Vec3 {
            x: 1.5,
            y: 2.5,
            z: 3.5,
        },
    );
    check(
        "mixed array struct",
        &Mixed {
            name: "x".into(),
            count: 3,
        },
    );
    check("vec of structs", &vec![reading("a", 3), reading("b", 0)]);
    check(
        "renamed key",
        &Outer {
            head: reading("head", 2),
            tail: vec![reading("tail", 1)],
            flag: true,
        },
    );
}

/// The one place a measurement could plausibly diverge from the writer: under
/// `SKIP_NULL` an object's member count is no longer a constant, and both the
/// count and the members it describes have to be measured the same way the
/// writer decides them.
#[test]
fn skipped_members() {
    #[derive(Default)]
    struct Sparse {
        a: Option<u8>,
        b: Option<String>,
        c: (),
        d: Box<Option<u32>>,
        e: u8,
    }
    structio::object!(Sparse { a, b, c, d, e });

    for (a, b, d) in [
        (None, None, None),
        (Some(1), None, None),
        (None, Some("x".to_string()), Some(4)),
        (Some(1), Some(String::new()), Some(0)),
    ] {
        check(
            "sparse",
            &Sparse {
                a,
                b,
                d: Box::new(d),
                ..Default::default()
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Default)]
enum Level {
    #[default]
    Info,
    Warning,
}
structio::unit_enum!(Level { Info, Warning });

#[derive(Default)]
enum Shape {
    #[default]
    Empty,
    Circle(f64),
    Label(String),
}
structio::tagged_enum!(Shape {
    Empty,
    Circle(_),
    Label(_),
});

#[test]
fn enums() {
    check("unit variant", &Level::Info);
    check("unit enum run", &vec![Level::Info, Level::Warning]);
    check("tagged unit", &Shape::Empty);
    check("tagged payload", &Shape::Circle(1.5));
    check("tagged string", &Shape::Label("outline".into()));
    check(
        "tagged run",
        &vec![Shape::Empty, Shape::Circle(0.0), Shape::Label("x".into())],
    );
}

// ---------------------------------------------------------------------------
// Extensions
// ---------------------------------------------------------------------------

#[test]
fn extensions() {
    check("complex f64", &Complex::new(1.0f64, -2.0));
    check("complex f32", &Complex::new(1.0f32, -2.0));
    check(
        "complex run",
        &vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, -4.0)],
    );
    check(
        "matrix",
        &Matrix::new(
            MatrixLayout::RowMajor,
            vec![2, 2],
            vec![1.0f64, 2.0, 3.0, 4.0],
        )
        .unwrap(),
    );
    check(
        "matrix of complex",
        &Matrix::new(
            MatrixLayout::ColumnMajor,
            vec![1, 2],
            vec![Complex::new(1.0f64, 2.0), Complex::new(3.0, -4.0)],
        )
        .unwrap(),
    );
    // Extents wide enough to leave the one-byte width the small case takes.
    check(
        "wide extents",
        &Matrix::new(MatrixLayout::RowMajor, vec![1, 300], vec![0u8; 300]).unwrap(),
    );
}

// ---------------------------------------------------------------------------
// Adapters
// ---------------------------------------------------------------------------

/// An adapter that writes something other than the type would, so a
/// measurement that quietly fell back to the type's own impl would be caught.
struct Millis;

impl beve::WriteAs<Duration> for Millis {
    fn write<O: Options>(value: &Duration, w: &mut beve::Writer<'_, O>) {
        beve::Write::write(&(value.as_millis() as u64), w);
    }

    fn is_null(value: &Duration) -> bool {
        value.is_zero()
    }
}

impl<'de> beve::ReadAs<'de, Duration> for Millis {
    fn read<O: Options>(
        value: &mut Duration,
        r: &mut beve::Reader<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut ms = 0u64;
        beve::Read::read(&mut ms, r)?;
        *value = Duration::from_millis(ms);
        Ok(())
    }
}

/// A key adapter, which decides the object header as well as the key bytes.
struct DottedKey;

impl beve::WriteKeyAs<Ipv4Addr> for DottedKey {
    const OBJECT: u8 = <String as beve::ToBeveKey>::OBJECT;

    fn write_key<O: Options>(value: &Ipv4Addr, w: &mut beve::Writer<'_, O>) {
        w.write_str_body(&value.to_string());
    }
}

impl beve::ReadKeyAs<Ipv4Addr> for DottedKey {
    fn from_key(key: beve::Key<'_>) -> Result<Ipv4Addr, ErrorCode> {
        match key {
            beve::Key::Str(s) => s.parse().map_err(|_| ErrorCode::InvalidNumber),
            _ => Err(ErrorCode::ExpectedString),
        }
    }
}

#[derive(Default)]
struct Timings {
    step: Duration,
    steps: Vec<Duration>,
    hosts: BTreeMap<Ipv4Addr, u16>,
}
structio::beve_object!(Timings {
    step as Millis,
    steps as Vec<Millis>,
    hosts as BTreeMap<DottedKey, structio::Same>,
});

#[test]
fn adapters() {
    check("adapters", &Timings::default());
    check(
        "adapters",
        &Timings {
            step: Duration::from_millis(1500),
            steps: vec![Duration::from_millis(1), Duration::from_secs(1)],
            hosts: BTreeMap::from([(Ipv4Addr::new(10, 0, 0, 1), 80u16)]),
        },
    );
}

// ---------------------------------------------------------------------------
// The compressed size's own widths
// ---------------------------------------------------------------------------

/// A length prefix is one, two, four or eight bytes depending on the value, so
/// a measurement that took the width from anywhere but the encoder would be
/// right on one side of a threshold and wrong on the other.
#[test]
fn size_prefix_thresholds() {
    for n in [0usize, 1, 62, 63, 64, 65, 16_382, 16_383, 16_384, 16_385] {
        check("string length", &"x".repeat(n));
        check("byte array length", &vec![0u8; n]);
        check("generic array length", &vec![Some(1u8); n.min(200)]);
    }
}

/// The member count crosses the same thresholds, though it takes a lot of
/// fields to do it. Sixty-four is the width step a real struct can reach.
#[test]
fn member_count_threshold() {
    // Sixty-five members, so the count needs two bytes rather than one.
    let wide: BTreeMap<String, u8> = (0..65u8).map(|i| (format!("k{i}"), i)).collect();
    check("wide map", &wide);
    let narrow: BTreeMap<String, u8> = (0..63u8).map(|i| (format!("k{i}"), i)).collect();
    check("narrow map", &narrow);
}

// ---------------------------------------------------------------------------
// The aligned form
// ---------------------------------------------------------------------------

/// Padding is chosen from the offset the payload would land at, so measuring
/// it means tracking that offset exactly. Walking a leading string through
/// every length up to a couple of alignment periods puts the payload at every
/// residue for each element width.
#[test]
fn aligned_padding_at_every_offset() {
    #[derive(Default)]
    struct Padded {
        lead: String,
        f64s: Vec<f64>,
        f32s: Vec<f32>,
        u16s: Vec<u16>,
        u128s: Vec<u128>,
        bytes: Vec<u8>,
        flags: Vec<bool>,
        names: Vec<String>,
    }
    structio::object!(Padded {
        lead,
        f64s,
        f32s,
        u16s,
        u128s,
        bytes,
        flags,
        names
    });

    for n in 0..40usize {
        let value = Padded {
            lead: "x".repeat(n),
            f64s: vec![1.5; 3],
            f32s: vec![2.5; 3],
            u16s: vec![7; 3],
            u128s: vec![9; 2],
            // One-byte elements and the categories with no width take the
            // plain form even when the aligned one is asked for.
            bytes: vec![1, 2, 3],
            flags: vec![true, false],
            names: vec!["a".into()],
        };
        check("padded", &value);

        // And the aligned form really is a different document here, or this
        // test would be measuring the plain one twice.
        assert_ne!(
            beve::to_vec_aligned(&value),
            beve::to_vec(&value),
            "the aligned form did not pad anything"
        );
    }
}

/// The padding is chosen from where the payload lands in the *document*, so a
/// value appended behind a prefix pads against that prefix. Every base offset
/// over two alignment periods, for every element width the form applies to.
///
/// The layout is recomputed here from the bytes rather than asked of the writer
/// a second time: a top-level aligned array is the marker, the element header,
/// a one-byte count, the stated padding length, and then the payload.
#[test]
fn aligned_padding_at_every_base() {
    macro_rules! width {
        ($ty:ty, $w:expr, $values:expr) => {
            for base in 0..40usize {
                let values: Vec<$ty> = $values.into();
                let mut frame = vec![0u8; base];
                beve::append_aligned(&values, &mut frame);

                let pad = frame[base + 3] as usize;
                let payload = base + 4 + pad;
                assert_eq!(
                    payload % $w,
                    0,
                    "{}s at base {base}: payload at {payload}",
                    stringify!($ty)
                );
                assert_eq!(frame.len(), payload + values.len() * $w);

                assert_eq!(
                    beve::size_aligned_after(&values, base),
                    frame.len() - base,
                    "{}s at base {base}",
                    stringify!($ty)
                );
                assert_eq!(
                    beve::from_slice::<Vec<$ty>>(&frame[base..]).unwrap(),
                    values,
                    "{}s at base {base}",
                    stringify!($ty)
                );

                // And the base is what moved it: the answer differs from the
                // one measured at zero for exactly the bases that are not
                // already on the element width.
                assert_eq!(
                    beve::size_aligned_after(&values, base) == beve::size_aligned(&values),
                    base % $w == 0,
                    "{}s at base {base}: measuring at zero would have done",
                    stringify!($ty)
                );
            }
        };
    }

    width!(f64, 8, [1.5f64, 2.5, 3.5]);
    width!(f32, 4, [1.5f32, 2.5, 3.5]);
    width!(u16, 2, [1u16, 2, 3]);
    width!(i32, 4, [-1i32, 2, -3]);
    width!(u128, 16, [1u128, 2, 3]);
}

/// The frame the offset exists for: a fixed header and a variable-length prefix
/// in front of an aligned body, whose length the header has to state before the
/// body is written and whose padding depends on both.
#[test]
fn a_frame_states_the_length_of_an_aligned_body() {
    const HEADER: usize = 48;

    for query_len in 0..40usize {
        let body = vec![1.5f64, 2.5, 3.5, 4.5];

        let mut frame = vec![0u8; HEADER];
        frame.extend_from_slice("q".repeat(query_len).as_bytes());
        let base = frame.len();

        // The length field, filled in before the body exists.
        let stated = beve::size_aligned_after(&body, base);
        frame[..8].copy_from_slice(&(stated as u64).to_le_bytes());

        beve::append_aligned(&body, &mut frame);
        assert_eq!(frame.len() - base, stated, "the header lied about the body");
        assert_eq!(
            (frame.len() - 32) % 8,
            0,
            "query {query_len}: the payload did not land on its element width"
        );

        // The same body through a sink told where it begins, buffered small
        // enough that a drain falls in the middle of it: the offset the padding
        // is chosen from has to survive the bytes leaving.
        let mut sunk = Vec::new();
        let mut w = beve::Writer::<Standard>::to_sink_with_capacity(&mut sunk, 4)
            .aligned()
            .at(base);
        beve::Write::write(&body, &mut w);
        w.finish().unwrap();
        assert_eq!(
            sunk.as_slice(),
            &frame[base..],
            "the sink laid the body out differently"
        );

        assert_eq!(beve::from_slice::<Vec<f64>>(&frame[base..]).unwrap(), body);
    }
}

/// `at` states where the writer stands, so telling it what its own buffer
/// already says changes nothing. That is the mistake worth forestalling: a
/// prefix counted once by the buffer and again by the offset lays the padding
/// out for a document nobody is writing.
#[test]
fn at_agrees_with_a_buffer_that_already_holds_the_prefix() {
    let samples = vec![1.5f64, 2.5, 3.5];

    for base in 0..40usize {
        let prefix = vec![0u8; base];

        let mut implied = prefix.clone();
        beve::append_aligned(&samples, &mut implied);

        let mut stated = beve::Writer::<Standard>::appending(prefix)
            .aligned()
            .at(base);
        beve::Write::write(&samples, &mut stated);
        assert_eq!(stated.into_vec(), implied, "base {base}");

        // And the same offset with nothing in the buffer is the body alone,
        // which is the form a sink or a separately assembled buffer takes.
        let mut alone = beve::Writer::<Standard>::new().aligned().at(base);
        beve::Write::write(&samples, &mut alone);
        assert_eq!(alone.offset(), implied.len(), "base {base}");
        assert_eq!(alone.into_vec(), implied[base..], "base {base}");
    }
}

/// A document may begin *inside* the buffer, which is what a send buffer
/// accumulating frames back to back makes of every frame after the first.
///
/// The receiver gets one frame, not the send buffer, so the padding has to be
/// chosen from the frame's own start. Appending without saying so pads against
/// the whole buffer instead, which is a document whose alignment is true of a
/// stream nobody reads. Both the equation and the landing place are checked
/// here, since this is the one position the buffer cannot imply.
#[test]
fn a_frame_appended_behind_earlier_frames_pads_against_itself() {
    let samples = vec![1.5f64, 2.5, 3.5];

    for queued in 0..40usize {
        for header in [0usize, 3, 8, 48] {
            let mut send = vec![0xAA; queued];
            let frame_start = send.len();
            send.extend_from_slice(&vec![0u8; header]);

            let at = send.len() - frame_start;
            let body = beve::size_aligned_after(&samples, at);

            let mut w = beve::Writer::<Standard>::appending(send).aligned().at(at);
            beve::Write::write(&samples, &mut w);
            let send = w.into_vec();

            let emitted = send.len() - frame_start - header;
            assert_eq!(emitted, body, "{queued} queued, {header}-byte header");

            // The payload is on its element width counted from this frame, and
            // the earlier frames are still where they were.
            let payload = send.len() - frame_start - samples.len() * 8;
            assert_eq!(payload % 8, 0, "{queued} queued, {header}-byte header");
            assert_eq!(send[..queued], vec![0xAA; queued][..]);
            assert_eq!(
                beve::from_slice::<Vec<f64>>(&send[frame_start + header..]).unwrap(),
                samples
            );

            // And it really is a different layout from the one the buffer would
            // have implied, wherever the two disagree about where zero is.
            let mut implied = vec![0xAA; queued];
            implied.extend_from_slice(&vec![0u8; header]);
            beve::append_aligned(&samples, &mut implied);
            assert_eq!(
                implied.len() == send.len(),
                frame_start % 8 == 0,
                "{queued} queued, {header}-byte header"
            );
        }
    }
}

/// Nothing built into this crate positions itself, so the plain form measures
/// the same wherever it lands. The aligned form is the one that moves.
#[test]
fn only_the_aligned_form_depends_on_where_it_lands() {
    let value = reading("sensor", 7);
    let flat = beve::size(&value);

    let mut moved = false;
    for base in 0..40usize {
        assert_eq!(beve::size_after(&value, base), flat, "the plain form moved");
        moved |= beve::size_aligned_after(&value, base) != beve::size_aligned(&value);
    }
    assert!(moved, "the aligned form never noticed the base offset");
}

// ---------------------------------------------------------------------------
// The whole point
// ---------------------------------------------------------------------------

/// The frame the measurement exists for: a header carrying the body's length,
/// written before the body, with the body going straight to the sink.
#[test]
fn frames_without_a_body_buffer() {
    let values = [reading("a", 100), reading("b", 0), reading("c", 3)];

    let mut wire = Vec::new();
    for value in &values {
        let body = beve::size(value);
        wire.extend_from_slice(&(body as u32).to_le_bytes());
        let before = wire.len();
        beve::to_writer(value, &mut wire).unwrap();
        assert_eq!(wire.len() - before, body, "the header lied about the body");
    }

    let mut rest = &wire[..];
    for value in &values {
        let (len, tail) = rest.split_at(4);
        let len = u32::from_le_bytes(len.try_into().unwrap()) as usize;
        let (frame, tail) = tail.split_at(len);
        assert_eq!(frame, beve::to_vec(value));
        rest = tail;
    }
    assert!(rest.is_empty());
}

// ---------------------------------------------------------------------------
// Laying a value out by hand
// ---------------------------------------------------------------------------

/// An implementation that positions its own bytes must ask `offset`, not `len`.
///
/// `len` is how much sits in the *buffer*, which a sink writer empties on every
/// drain and a measuring writer never fills, so a value laid out from it comes
/// out differently under all three writers. `offset` is the position in the
/// document and is correct under each. This is the one way a hand-written impl
/// could measure differently from the way it writes, so it is pinned here
/// rather than left to the prose.
#[test]
fn a_hand_laid_out_value_measures_from_offset() {
    use structio::beve::header;

    /// The aligned preamble spelled out, which `Writer::aligned`'s own
    /// documentation contemplates an implementation doing.
    struct SelfAligned(Vec<f64>);

    impl beve::Write for SelfAligned {
        fn write<O: Options>(&self, w: &mut beve::Writer<'_, O>) {
            w.push(header::ALIGNED_ARRAY);
            w.push(header::array_of(header::CAT_FLOAT, 3));
            w.size(self.0.len() as u64);
            let pad = 8 - 1 - w.offset() % 8;
            w.push(pad as u8);
            for _ in 0..pad {
                w.push(0);
            }
            for v in &self.0 {
                w.raw(&v.to_le_bytes());
            }
        }
    }

    for n in 0..40usize {
        let value = ("x".repeat(n), SelfAligned(vec![1.0, 2.0, 3.0]));
        check("self-aligned", &value);

        // The layout it computed is the one it meant, under each writer, which
        // is what makes the measurement worth comparing against at all.
        let flat = beve::to_vec(&value);
        let mut sunk = Vec::new();
        beve::to_writer_buffered(&value, &mut sunk, 4).unwrap();
        assert_eq!(sunk, flat, "a drain moved the hand-written layout");
        assert_eq!(
            (flat.len() - 24) % 8,
            0,
            "the payload did not land on its element width"
        );
    }

    // And the same impl behind a prefix. It positions itself from `offset`, so
    // this is the plain form measuring differently at different bases, which
    // nothing built into the crate does.
    let value = SelfAligned(vec![1.0, 2.0, 3.0]);
    let mut moved = false;
    for base in 0..40usize {
        check_after::<Standard, _>("self-aligned", &value, base);

        let mut frame = vec![0u8; base];
        beve::append(&value, &mut frame);
        assert_eq!(
            (frame.len() - 24) % 8,
            0,
            "base {base}: the payload did not land on its element width"
        );
        moved |= beve::size_after(&value, base) != beve::size(&value);
    }
    assert!(
        moved,
        "an impl reading `offset` measured the same everywhere"
    );
}
