//! Field adapters: describing a type you do not own without wrapping it.
//!
//! The types adapted here are `std::time::Duration` and `std::net::Ipv4Addr`,
//! which this crate deliberately does not describe, and `Vec<u8>` and `&str`,
//! which it does. The first two stand in for the foreign types the mechanism
//! exists for; the second two are here because an adapter over a type the crate
//! already knows is the case where a wrong answer is checkable against a right
//! one. Nothing in a declaration knows the difference.

use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;
use std::time::Duration;

use structio::{
    ErrorCode, Options, Same, SkipNull, beve, from_beve, from_str, json, to_beve, to_string,
    to_string_with,
};

// ---------------------------------------------------------------------------
// Adapters under test
// ---------------------------------------------------------------------------

/// A `Duration` as a whole number of milliseconds, calling a zero duration
/// absent so that `SKIP_NULL` has something to drop that the type itself would
/// have written.
struct Millis;

impl<'de> json::ReadAs<'de, Duration> for Millis {
    fn read<O: Options>(
        value: &mut Duration,
        p: &mut json::Parser<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut ms = 0u64;
        json::Read::read(&mut ms, p)?;
        *value = Duration::from_millis(ms);
        Ok(())
    }
}

impl json::WriteAs<Duration> for Millis {
    fn write<O: Options>(value: &Duration, w: &mut json::Writer<'_, O>) {
        json::Write::write(&(value.as_millis() as u64), w);
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

impl beve::WriteAs<Duration> for Millis {
    fn write<O: Options>(value: &Duration, w: &mut beve::Writer<'_, O>) {
        beve::Write::write(&(value.as_millis() as u64), w);
    }

    fn is_null(value: &Duration) -> bool {
        value.is_zero()
    }
}

/// An `Ipv4Addr` as dotted-quad text. JSON only, to check that a declaration
/// asks for exactly the halves its format set needs.
struct Dotted;

impl<'de> json::ReadAs<'de, Ipv4Addr> for Dotted {
    fn read<O: Options>(
        value: &mut Ipv4Addr,
        p: &mut json::Parser<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut s = String::new();
        json::Read::read(&mut s, p)?;
        *value = s.parse().map_err(|_| ErrorCode::InvalidNumber)?;
        Ok(())
    }
}

impl json::WriteAs<Ipv4Addr> for Dotted {
    fn write<O: Options>(value: &Ipv4Addr, w: &mut json::Writer<'_, O>) {
        w.write_str(&value.to_string());
    }
}

/// A whole `Vec<u8>` as one hex string, not an adapter over its elements.
///
/// It sits beside `Vec<Millis>` in the same declaration below, which is the
/// case a blanket impl on the bare adapter would have foreclosed.
struct Hex;

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Refills `out` rather than replacing it, which is what a `ReadAs` impl owes
/// its caller.
fn from_hex(s: &str, out: &mut Vec<u8>) -> Result<(), ErrorCode> {
    fn digit(b: u8) -> Result<u8, ErrorCode> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(ErrorCode::InvalidNumber),
        }
    }
    if !s.len().is_multiple_of(2) {
        return Err(ErrorCode::InvalidNumber);
    }
    out.clear();
    for pair in s.as_bytes().chunks(2) {
        out.push(digit(pair[0])? << 4 | digit(pair[1])?);
    }
    Ok(())
}

impl<'de> json::ReadAs<'de, Vec<u8>> for Hex {
    fn read<O: Options>(
        value: &mut Vec<u8>,
        p: &mut json::Parser<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut s = String::new();
        json::Read::read(&mut s, p)?;
        from_hex(&s, value)
    }
}

impl json::WriteAs<Vec<u8>> for Hex {
    fn write<O: Options>(value: &Vec<u8>, w: &mut json::Writer<'_, O>) {
        w.write_str(&to_hex(value));
    }
}

impl<'de> beve::ReadAs<'de, Vec<u8>> for Hex {
    fn read<O: Options>(
        value: &mut Vec<u8>,
        r: &mut beve::Reader<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut s = String::new();
        beve::Read::read(&mut s, r)?;
        from_hex(&s, value)
    }
}

impl beve::WriteAs<Vec<u8>> for Hex {
    fn write<O: Options>(value: &Vec<u8>, w: &mut beve::Writer<'_, O>) {
        w.write_str(&to_hex(value));
    }
}

// ---------------------------------------------------------------------------
// A leaf adapter, in both formats
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Event {
    id: u32,
    elapsed: Duration,
}
structio::object!(Event {
    id,
    "elapsed_ms" => elapsed as Millis,
});

#[test]
fn a_leaf_adapter_round_trips_in_json() {
    let e = Event {
        id: 7,
        elapsed: Duration::from_millis(1500),
    };
    let text = to_string(&e);
    assert_eq!(text, r#"{"id":7,"elapsed_ms":1500}"#);
    assert_eq!(from_str::<Event>(&text).unwrap(), e);
}

#[test]
fn a_leaf_adapter_round_trips_in_beve() {
    let e = Event {
        id: 7,
        elapsed: Duration::from_millis(1500),
    };
    let bytes = to_beve(&e);
    assert_eq!(from_beve::<Event>(&bytes).unwrap(), e);
}

#[test]
fn an_adapted_field_keeps_its_renamed_key() {
    let e = Event {
        id: 1,
        elapsed: Duration::from_millis(2),
    };
    // The Rust name is not a key any more, in either direction.
    assert_eq!(to_string(&e), r#"{"id":1,"elapsed_ms":2}"#);
    assert!(from_str::<Event>(r#"{"id":1,"elapsed":2}"#).is_err());
}

// ---------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Timings {
    one: Option<Duration>,
    all: Vec<Duration>,
    maybe_all: Option<Vec<Duration>>,
    all_maybe: Vec<Option<Duration>>,
    fixed: [Duration; 2],
}
structio::object!(Timings {
    one as Option<Millis>,
    all as Vec<Millis>,
    maybe_all as Option<Vec<Millis>>,
    all_maybe as Vec<Option<Millis>>,
    fixed as [Millis; 2],
});

fn timings() -> Timings {
    Timings {
        one: Some(Duration::from_millis(5)),
        all: vec![Duration::from_millis(1), Duration::from_millis(2)],
        maybe_all: Some(vec![Duration::from_millis(3)]),
        all_maybe: vec![None, Some(Duration::from_millis(4))],
        fixed: [Duration::from_millis(6), Duration::from_millis(7)],
    }
}

#[test]
fn adapters_compose_through_containers() {
    let t = timings();
    let text = to_string(&t);
    assert_eq!(
        text,
        r#"{"one":5,"all":[1,2],"maybe_all":[3],"all_maybe":[null,4],"fixed":[6,7]}"#
    );
    assert_eq!(from_str::<Timings>(&text).unwrap(), t);

    let bytes = to_beve(&t);
    assert_eq!(from_beve::<Timings>(&bytes).unwrap(), t);
}

#[test]
fn an_absent_option_reads_back_absent() {
    let text = r#"{"one":null,"all":[],"maybe_all":null,"all_maybe":[],"fixed":[0,0]}"#;
    let t: Timings = from_str(text).unwrap();
    assert_eq!(t.one, None);
    assert_eq!(t.maybe_all, None);
    assert_eq!(to_string(&t), text);
}

#[derive(Default, Debug, PartialEq)]
struct Mixed {
    blob: Vec<u8>,
    steps: Vec<Duration>,
}
// A whole-container adapter beside an element-wise one, which is what having
// adapters compose as types rather than by a blanket impl buys.
structio::object!(Mixed {
    blob as Hex,
    steps as Vec<Millis>,
});

#[test]
fn a_whole_container_adapter_sits_beside_an_element_wise_one() {
    let m = Mixed {
        blob: vec![0xde, 0xad, 0xbe, 0xef],
        steps: vec![Duration::from_millis(9)],
    };
    let text = to_string(&m);
    assert_eq!(text, r#"{"blob":"deadbeef","steps":[9]}"#);
    assert_eq!(from_str::<Mixed>(&text).unwrap(), m);
    assert_eq!(from_beve::<Mixed>(&to_beve(&m)).unwrap(), m);
}

// ---------------------------------------------------------------------------
// `Same`
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Plain {
    n: u32,
    xs: Vec<f64>,
    label: String,
}
structio::object!(Plain { n, xs, label });

#[derive(Default, Debug, PartialEq)]
struct Identity {
    n: u32,
    xs: Vec<f64>,
    label: String,
}
structio::object!(Identity {
    n as Same,
    xs as Vec<Same>,
    label as Same,
});

fn pair() -> (Plain, Identity) {
    (
        Plain {
            n: 3,
            xs: vec![1.0, 2.0, 3.0],
            label: "hi".into(),
        },
        Identity {
            n: 3,
            xs: vec![1.0, 2.0, 3.0],
            label: "hi".into(),
        },
    )
}

#[test]
fn same_writes_what_the_type_would_in_json() {
    let (plain, identity) = pair();
    assert_eq!(to_string(&plain), to_string(&identity));
}

#[test]
fn same_writes_what_the_type_would_in_beve() {
    let (plain, identity) = pair();
    // Byte identity, not merely a round trip: a `Vec<Same>` that lost the typed
    // array would still read back correctly and the document would just be
    // three bytes longer, which nothing else here would notice.
    assert_eq!(to_beve(&plain), to_beve(&identity));
    assert_eq!(from_beve::<Identity>(&to_beve(&plain)).unwrap(), identity);
}

#[test]
fn an_adapted_sequence_reuses_the_elements_it_holds() {
    #[derive(Default)]
    struct Holder {
        items: Vec<String>,
    }
    structio::object!(Holder {
        items as Vec<Same>,
    });

    let mut h = Holder::default();
    structio::read_into(&mut h, r#"{"items":["alpha","beta"]}"#).unwrap();
    let before: Vec<(*const u8, usize)> =
        h.items.iter().map(|s| (s.as_ptr(), s.capacity())).collect();

    structio::read_into(&mut h, r#"{"items":["gamma","delta"]}"#).unwrap();
    let after: Vec<(*const u8, usize)> =
        h.items.iter().map(|s| (s.as_ptr(), s.capacity())).collect();

    assert_eq!(h.items, ["gamma", "delta"]);
    assert_eq!(before, after, "the adapted read replaced the buffers");
}

// ---------------------------------------------------------------------------
// One format at a time
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
struct Host {
    addr: Ipv4Addr,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            addr: Ipv4Addr::UNSPECIFIED,
        }
    }
}

// `Dotted` has no BEVE half, so this has to be a `json_object!`. That it
// compiles is the test: an `object!` here would demand both halves.
structio::json_object!(Host { addr as Dotted });

#[test]
fn a_json_only_adapter_declares_a_json_only_struct() {
    let h = Host {
        addr: Ipv4Addr::new(10, 0, 0, 1),
    };
    let text = to_string(&h);
    assert_eq!(text, r#"{"addr":"10.0.0.1"}"#);
    assert_eq!(from_str::<Host>(&text).unwrap(), h);
}

// ---------------------------------------------------------------------------
// Generic and borrowing declarations
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Page<T> {
    items: Vec<T>,
    took: Duration,
}
structio::object!([T: structio::ReadWrite + Default] Page<T> {
    items,
    took as Millis,
});

#[test]
fn an_adapter_works_on_a_generic_declaration() {
    let p = Page {
        items: vec![1u32, 2, 3],
        took: Duration::from_millis(11),
    };
    let text = to_string(&p);
    assert_eq!(text, r#"{"items":[1,2,3],"took":11}"#);
    assert_eq!(from_str::<Page<u32>>(&text).unwrap(), p);
}

/// An adapter that borrows from the document, to check that the `'_` the macro
/// writes binds to the declaration's own `'de`.
struct Trimmed;

impl<'de> json::ReadAs<'de, &'de str> for Trimmed {
    fn read<O: Options>(
        value: &mut &'de str,
        p: &mut json::Parser<'de, O>,
    ) -> Result<(), ErrorCode> {
        let mut s: &'de str = "";
        json::Read::read(&mut s, p)?;
        *value = s.trim();
        Ok(())
    }
}

impl json::WriteAs<&str> for Trimmed {
    fn write<O: Options>(value: &&str, w: &mut json::Writer<'_, O>) {
        w.write_str(value.trim());
    }
}

#[derive(Default, Debug, PartialEq)]
struct Borrowing<'a> {
    name: &'a str,
}
structio::json_object!(['de] Borrowing<'de> { name as Trimmed });

#[test]
fn an_adapter_can_borrow_from_the_document() {
    let doc = r#"{"name":"  padded  "}"#;
    let b: Borrowing<'_> = from_str(doc).unwrap();
    assert_eq!(b.name, "padded");
    // Borrowed, not copied: the field points into the document itself.
    assert!(doc.as_bytes().as_ptr_range().contains(&b.name.as_ptr()));
}

// ---------------------------------------------------------------------------
// Maps
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Budgets {
    by_id: HashMap<u32, Duration>,
    by_name: BTreeMap<String, Duration>,
}
structio::object!(Budgets {
    by_id as HashMap<Same, Millis>,
    by_name as BTreeMap<Same, Millis>,
});

#[test]
fn a_map_adapts_its_values_and_leaves_its_keys() {
    let mut b = Budgets::default();
    b.by_id.insert(4, Duration::from_millis(40));
    b.by_name.insert("slow".into(), Duration::from_millis(9000));

    let text = to_string(&b);
    // The integer key is still quoted, which is the only form JSON has.
    assert!(text.contains(r#""4":40"#));
    assert!(text.contains(r#""slow":9000"#));
    assert_eq!(from_str::<Budgets>(&text).unwrap(), b);

    let bytes = to_beve(&b);
    assert_eq!(from_beve::<Budgets>(&bytes).unwrap(), b);
}

#[test]
fn an_adapted_map_keeps_the_object_header_its_keys_imply() {
    #[derive(Default)]
    struct Plain {
        m: HashMap<u32, u64>,
    }
    structio::object!(Plain { m });

    #[derive(Default)]
    struct Adapted {
        m: HashMap<u32, u64>,
    }
    structio::object!(Adapted {
        m as HashMap<Same, Same>,
    });

    let mut plain = Plain::default();
    plain.m.insert(1, 2);
    let mut adapted = Adapted::default();
    adapted.m.insert(1, 2);

    // An integer-keyed object has a header of its own, and it comes from the
    // key adapter rather than from the key type.
    assert_eq!(to_beve(&plain), to_beve(&adapted));
}

// ---------------------------------------------------------------------------
// `SKIP_NULL`, and the BEVE member count that has to agree with it
// ---------------------------------------------------------------------------

#[derive(Default, Debug, PartialEq)]
struct Report {
    id: u32,
    idle: Duration,
    limit: Option<Duration>,
}
structio::object!(Report {
    id,
    idle as Millis,
    limit as Option<Millis>,
});

#[test]
fn skip_null_drops_a_member_the_adapter_calls_absent() {
    let r = Report {
        id: 1,
        idle: Duration::ZERO,
        limit: None,
    };
    assert_eq!(to_string_with::<SkipNull, _>(&r), r#"{"id":1}"#);

    let present = Report {
        id: 1,
        idle: Duration::from_millis(3),
        limit: Some(Duration::from_millis(4)),
    };
    assert_eq!(
        to_string_with::<SkipNull, _>(&present),
        r#"{"id":1,"idle":3,"limit":4}"#
    );
}

/// The BEVE member count is stated in the object header before any member is
/// written, so a `count_fields` that disagreed with `write_fields` would
/// produce a document a reader misparses rather than rejects. The debug
/// assertion catches it, and this test is deliberately not gated on
/// `debug_assertions`: gating it would delete it from the release run, where
/// the round trip below is what stands in for the assertion.
#[test]
fn the_beve_member_count_follows_the_adapter() {
    for r in [
        Report {
            id: 1,
            idle: Duration::ZERO,
            limit: None,
        },
        Report {
            id: 2,
            idle: Duration::from_millis(5),
            limit: None,
        },
        Report {
            id: 3,
            idle: Duration::ZERO,
            limit: Some(Duration::from_millis(6)),
        },
        Report {
            id: 4,
            idle: Duration::from_millis(7),
            limit: Some(Duration::from_millis(8)),
        },
    ] {
        let bytes = structio::to_beve_with::<SkipNull, _>(&r);
        let back: Report = from_beve(&bytes).unwrap();
        assert_eq!(back, r);
    }
}

// ---------------------------------------------------------------------------
// A key adapter for a key type nobody owns
// ---------------------------------------------------------------------------
//
// `Same` covers a key whose type already implements `FromJsonKey`/`FromBeveKey`.
// This is the case it cannot reach and the key traits exist for: `Ipv4Addr` is
// not a key type this crate knows, and the orphan rule blocks making it one.

struct DottedKey;

impl json::ReadKeyAs<Ipv4Addr> for DottedKey {
    fn from_key(key: &str) -> Result<Ipv4Addr, ErrorCode> {
        key.parse().map_err(|_| ErrorCode::InvalidNumber)
    }
}

impl json::WriteKeyAs<Ipv4Addr> for DottedKey {
    fn write_key<O: Options>(value: &Ipv4Addr, w: &mut json::Writer<'_, O>) {
        w.write_str(&value.to_string());
    }
}

impl beve::ReadKeyAs<Ipv4Addr> for DottedKey {
    fn from_key(key: beve::Key<'_>) -> Result<Ipv4Addr, ErrorCode> {
        match key {
            beve::Key::Str(s) => s.parse().map_err(|_| ErrorCode::InvalidNumber),
            _ => Err(ErrorCode::UnsupportedKeyType),
        }
    }
}

impl beve::WriteKeyAs<Ipv4Addr> for DottedKey {
    // A string-keyed object, though the key type is not a string.
    const OBJECT: u8 = beve::header::OBJECT;

    fn write_key<O: Options>(value: &Ipv4Addr, w: &mut beve::Writer<'_, O>) {
        w.write_str_body(&value.to_string());
    }
}

#[derive(Default, Debug, PartialEq)]
struct Routes {
    hops: BTreeMap<Ipv4Addr, Duration>,
}
structio::object!(Routes {
    hops as BTreeMap<DottedKey, Millis>,
});

#[test]
fn a_key_adapter_reaches_a_key_type_the_crate_does_not_know() {
    let mut r = Routes::default();
    r.hops
        .insert(Ipv4Addr::new(10, 0, 0, 1), Duration::from_millis(12));
    r.hops
        .insert(Ipv4Addr::new(10, 0, 0, 2), Duration::from_millis(34));

    let text = to_string(&r);
    assert_eq!(text, r#"{"hops":{"10.0.0.1":12,"10.0.0.2":34}}"#);
    assert_eq!(from_str::<Routes>(&text).unwrap(), r);
    assert_eq!(from_beve::<Routes>(&to_beve(&r)).unwrap(), r);
}

// ---------------------------------------------------------------------------
// A leaf adapter carrying a typed array of its own
// ---------------------------------------------------------------------------
//
// `Same` forwards `ARRAY`, so every other test here reaches the typed-array
// branch of `write_slice_with` only through the element's own `Write`. This is
// the branch as a third-party adapter would use it: `char` has no BEVE typed
// array, and `CharCode` gives its runs the one `u32` has.

struct CharCode;

impl<'de> beve::ReadAs<'de, char> for CharCode {
    fn read<O: Options>(value: &mut char, r: &mut beve::Reader<'de, O>) -> Result<(), ErrorCode> {
        let mut code = 0u32;
        beve::Read::read(&mut code, r)?;
        *value = char::from_u32(code).ok_or(ErrorCode::InvalidUtf8)?;
        Ok(())
    }
}

impl beve::WriteAs<char> for CharCode {
    fn write<O: Options>(value: &char, w: &mut beve::Writer<'_, O>) {
        beve::Write::write(&(*value as u32), w);
    }

    const ARRAY: Option<&'static [u8]> = <u32 as beve::Write>::ARRAY;

    fn write_payload<O: Options>(items: &[char], w: &mut beve::Writer<'_, O>) {
        let codes: Vec<u32> = items.iter().map(|c| *c as u32).collect();
        <u32 as beve::Write>::write_payload(&codes, w);
    }
}

#[derive(Default, Debug, PartialEq)]
struct Glyphs {
    codes: Vec<char>,
}
structio::beve_object!(Glyphs {
    codes as Vec<CharCode>,
});

#[derive(Default)]
struct RawCodes {
    codes: Vec<u32>,
}
structio::beve_object!(RawCodes { codes });

#[test]
fn an_adapter_can_declare_a_typed_array_of_its_own() {
    let g = Glyphs {
        codes: vec!['a', 'b', 'c'],
    };
    let raw = RawCodes {
        codes: vec![97, 98, 99],
    };
    // One header and one block, not a value per element: byte for byte the
    // `Vec<u32>` the adapter says these are.
    assert_eq!(to_beve(&g), to_beve(&raw));
    assert_eq!(from_beve::<Glyphs>(&to_beve(&g)).unwrap(), g);
}
