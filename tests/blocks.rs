//! The typed-array path, reached from outside the crate.
//!
//! BEVE stores a run of numbers as one header and one contiguous block, which
//! is most of why the format is worth having. A type this crate describes gets
//! that from `beve::Write::ARRAY` and `beve::Read::read_bulk`; the subject here
//! is everything that has to hold for a type it does *not* describe to get the
//! same thing through an adapter.
//!
//! `Celsius` stands in for a scalar from a crate nobody here owns: it is
//! `#[repr(transparent)]` over an `f64`, and the orphan rule is imagined to
//! keep `Read` and `Write` off it, so a field holding one is declared through
//! an adapter. Everything asserted about it is asserted against the same field
//! declared as a plain `Vec<f64>`, which is the answer an adapter has to match
//! rather than merely round-trip against.
//!
//! One refusal here is a compile error and so cannot be a `#[test]`: a
//! `NumericBytes` impl whose declared element is not its own width. It is
//! checked by hand, this crate having no `trybuild` harness.
//!
//! # Little-endian only, deliberately
//!
//! A block is the payload reinterpreted as values, so it can be taken whole
//! only where the stored little-endian bytes are already in the host's order.
//! Every impl in the crate says so at the top of `read_bulk`, and so does the
//! adapter below.
//!
//! That makes this whole file a little-endian property, and it is gated as
//! [`tests/borrow.rs`](borrow.rs) is and for the same two reasons. The
//! positive cases cannot hold on big-endian by construction. And the negative
//! ones -- a generic array declining, a stored `f32` declining, a string array
//! declining -- would still *pass* there, having declined for the endianness
//! rather than for the reason each exists to pin, which is worse than not
//! running.
//!
//! What holds on every target is asserted in `tests/blocks_big_endian.rs`:
//! the adapted document is still byte for byte the bare type's, and it still
//! reads back.
#![cfg(target_endian = "little")]

use structio::beve::{self, NumericBytes, Read, ReadAs, Reader, Write, WriteAs, Writer, header};
use structio::{ErrorCode, Options, Same, from_beve, to_beve};

type PResult<T> = Result<T, ErrorCode>;

// ---------------------------------------------------------------------------
// A foreign scalar, and the adapter that describes it
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
struct Celsius(f64);

// SAFETY: `repr(transparent)` over `f64`, which occupies eight initialized
// bytes with no padding, takes every bit pattern of that size, and is stored
// little endian by BEVE. The declared element is `f64`'s own, so one of these
// is one element of payload.
unsafe impl NumericBytes for Celsius {
    const ELEMENT: u8 = <f64 as NumericBytes>::ELEMENT;
}

/// The whole adapter: a value at a time in both directions, and a block at a
/// time in both directions.
struct AsDegrees;

impl<'de> ReadAs<'de, Celsius> for AsDegrees {
    fn read<O: Options>(value: &mut Celsius, r: &mut Reader<'de, O>) -> PResult<()> {
        value.0.read(r)
    }

    fn read_bulk<O: Options>(
        out: &mut Vec<Celsius>,
        n: usize,
        elem: u8,
        r: &mut Reader<'de, O>,
    ) -> PResult<bool> {
        // The half of the contract the caller cannot check: that the stored
        // element type is this one, and that the host stores it the way the
        // wire does.
        if elem != <Celsius as NumericBytes>::ELEMENT || cfg!(target_endian = "big") {
            return Ok(false);
        }
        r.read_block(out, n)?;
        Ok(true)
    }
}

impl WriteAs<Celsius> for AsDegrees {
    fn write<O: Options>(value: &Celsius, w: &mut Writer<'_, O>) {
        value.0.write(w);
    }

    const ARRAY: Option<&'static [u8]> = <f64 as Write>::ARRAY;

    fn write_payload<O: Options>(items: &[Celsius], w: &mut Writer<'_, O>) {
        if cfg!(target_endian = "little") {
            w.write_block(items);
        } else {
            for c in items {
                w.raw(&c.0.to_le_bytes());
            }
        }
    }
}

#[derive(Default, Debug, PartialEq)]
struct Reading {
    samples: Vec<Celsius>,
}
// BEVE only: the typed array is a BEVE notion, and JSON has one array syntax
// that neither half of this could change.
structio::beve_object!(Reading { samples as Vec<AsDegrees> });

#[derive(Default, Debug, PartialEq)]
struct Plain {
    samples: Vec<f64>,
}
structio::beve_object!(Plain { samples });

fn degrees() -> Vec<Celsius> {
    vec![Celsius(-40.0), Celsius(0.5), Celsius(21.0), Celsius(100.0)]
}

fn plain() -> Vec<f64> {
    vec![-40.0, 0.5, 21.0, 100.0]
}

// ---------------------------------------------------------------------------
// The round trip a foreign scalar could not have before
// ---------------------------------------------------------------------------

#[test]
fn an_adapted_foreign_scalar_writes_the_typed_array_the_bare_type_would() {
    // Byte identity, not a round trip. A generic array would read back
    // correctly and be one byte per element larger, which is exactly the
    // difference a pinned byte contract cares about and nothing else notices.
    assert_eq!(
        to_beve(&Reading { samples: degrees() }),
        to_beve(&Plain { samples: plain() })
    );
}

#[test]
fn an_adapted_foreign_scalar_reads_the_typed_array_back() {
    let doc = to_beve(&Plain { samples: plain() });
    assert_eq!(
        from_beve::<Reading>(&doc).unwrap(),
        Reading { samples: degrees() }
    );
}

/// `AsDegrees` with its element path taken away, so that reading through it at
/// all is proof the block path ran.
///
/// Everything asserted through it would still pass if `read_bulk` were never
/// called and the elements were read one at a time, the values being the same
/// either way. Refusing to read an element is what turns those into tests of
/// the path rather than of the answer.
struct BlockOnly;

impl<'de> ReadAs<'de, Celsius> for BlockOnly {
    fn read<O: Options>(_: &mut Celsius, _: &mut Reader<'de, O>) -> PResult<()> {
        Err(ErrorCode::ExpectedNumber)
    }

    fn read_bulk<O: Options>(
        out: &mut Vec<Celsius>,
        n: usize,
        elem: u8,
        r: &mut Reader<'de, O>,
    ) -> PResult<bool> {
        <AsDegrees as ReadAs<'de, Celsius>>::read_bulk(out, n, elem, r)
    }
}

impl WriteAs<Celsius> for BlockOnly {
    fn write<O: Options>(value: &Celsius, w: &mut Writer<'_, O>) {
        <AsDegrees as WriteAs<Celsius>>::write(value, w);
    }
}

#[derive(Default, Debug, PartialEq)]
struct Strict {
    samples: Vec<Celsius>,
}
structio::beve_object!(Strict { samples as Vec<BlockOnly> });

#[test]
fn the_block_read_is_the_one_that_ran() {
    let doc = to_beve(&Plain { samples: plain() });
    assert_eq!(
        from_beve::<Strict>(&doc).unwrap(),
        Strict { samples: degrees() }
    );
}

#[test]
fn the_aligned_form_reaches_the_adapter_too() {
    // The aligned form states its element type in a second header and pads the
    // payload, so the preamble walk takes its own branch to reach the block.
    // That branch is the one a bulk path most easily loses, the form existing
    // precisely to be taken whole, so it is asserted through the adapter that
    // cannot read elements.
    let aligned = structio::to_beve_aligned(&Plain { samples: plain() });
    assert_ne!(
        aligned,
        to_beve(&Plain { samples: plain() }),
        "not the aligned form, so this asserts nothing"
    );
    assert_eq!(
        from_beve::<Strict>(&aligned).unwrap(),
        Strict { samples: degrees() }
    );
}

#[test]
fn an_adapter_that_consumes_and_then_declines_is_corrected() {
    // The contract says not to do this. The caller does not take its word for
    // it: the cursor is put back on the way to `false` rather than assumed to
    // have stayed put, so a wrong adapter costs a wasted copy rather than a
    // document read on from the middle of a payload.
    struct Greedy;

    impl<'de> ReadAs<'de, Celsius> for Greedy {
        fn read<O: Options>(value: &mut Celsius, r: &mut Reader<'de, O>) -> PResult<()> {
            value.0.read(r)
        }

        fn read_bulk<O: Options>(
            out: &mut Vec<Celsius>,
            n: usize,
            _elem: u8,
            r: &mut Reader<'de, O>,
        ) -> PResult<bool> {
            r.read_block(out, n)?;
            Ok(false)
        }
    }

    let doc = to_beve(&Plain { samples: plain() });
    let mut r = Reader::new(&doc);
    r.seek("/samples").unwrap();
    let at = r.position();

    let mut out: Vec<Celsius> = Vec::new();
    assert!(!r.try_bulk_with::<Greedy, _>(&mut out).unwrap());
    assert_eq!(r.position(), at, "declining left the cursor moved");

    // And the array is still all there to be read the ordinary way.
    <Vec<Greedy> as ReadAs<'_, Vec<Celsius>>>::read(&mut out, &mut r).unwrap();
    assert_eq!(out, degrees());
}

#[test]
fn an_adapter_that_declines_falls_back_to_its_element_path() {
    // The same declaration against a generic array, which no block hook can
    // take. The element path is what reads it, so `BlockOnly` would fail here
    // and `AsDegrees` must not.
    let mut w: Writer = Writer::new();
    w.begin_generic_array(plain().len());
    for x in plain() {
        w.element(&x);
    }
    let array = w.into_vec();

    let mut r = Reader::new(&array);
    let mut out: Vec<Celsius> = Vec::new();
    assert!(
        !r.try_bulk_with::<AsDegrees, _>(&mut out).unwrap(),
        "a generic array is not a block"
    );
    assert_eq!(r.position(), 0, "declining consumed input");

    <Vec<AsDegrees> as ReadAs<'_, Vec<Celsius>>>::read(&mut out, &mut r).unwrap();
    assert_eq!(out, degrees());
}

#[test]
fn an_adapter_declines_a_block_of_another_element_type() {
    // `f32` where the adapter wants `f64`. Widening is a conversion, and a
    // conversion is not a copy, so the block is refused and the element path
    // does the widening one value at a time.
    #[derive(Default)]
    struct Narrow {
        samples: Vec<f32>,
    }
    structio::beve_object!(Narrow { samples });

    let doc = to_beve(&Narrow {
        samples: vec![-40.0, 0.5],
    });
    let mut r = Reader::new(&doc);
    r.seek("/samples").unwrap();

    let mut out: Vec<Celsius> = Vec::new();
    assert!(!r.try_bulk_with::<AsDegrees, _>(&mut out).unwrap());

    assert_eq!(
        from_beve::<Reading>(&doc).unwrap().samples,
        [Celsius(-40.0), Celsius(0.5)]
    );
}

// ---------------------------------------------------------------------------
// `Same` keeps both halves
// ---------------------------------------------------------------------------

#[test]
fn same_forwards_the_block_read() {
    let doc = to_beve(&plain());

    let mut r = Reader::new(&doc);
    let mut out: Vec<f64> = Vec::new();
    assert!(r.try_bulk_with::<Same, _>(&mut out).unwrap());
    assert_eq!(out, plain());
    r.finish().expect("the block was not consumed whole");
}

#[test]
fn same_reads_a_block_where_the_bare_type_does_and_declines_where_it_does_not() {
    // The identity claim in both directions: whatever `Read::read_bulk` would
    // have answered is what the adapted form answers.
    let strings = to_beve(&vec![String::from("a"), String::from("b")]);
    let mut r = Reader::new(&strings);
    let mut out: Vec<String> = Vec::new();
    assert!(
        !r.try_bulk_with::<Same, _>(&mut out).unwrap(),
        "a string array is not a numeric block"
    );
    assert_eq!(r.position(), 0);
}

// ---------------------------------------------------------------------------
// The block helpers themselves
// ---------------------------------------------------------------------------

#[test]
fn a_block_written_by_hand_is_the_document_the_crate_writes() {
    // The two public halves used directly, without an adapter in the way: open
    // the array the element header names, then append the payload.
    let items = degrees();
    let array = <f64 as Write>::ARRAY.expect("f64 has a typed array");

    let mut w: Writer = Writer::new();
    w.begin_typed_array(array[0], items.len());
    w.write_block(&items);
    let doc = w.into_vec();

    assert_eq!(doc, to_beve(&plain()));
    // What the writer opened is the array whose elements are what `Celsius`
    // declared, which is the pairing `read_bulk` is handed to check.
    assert_eq!(
        header::element_of(doc[0]),
        <Celsius as NumericBytes>::ELEMENT
    );
}

#[test]
fn a_block_read_replaces_what_the_vector_held() {
    let doc = to_beve(&plain());
    let mut out = vec![Celsius(f64::NAN); 32];
    let mut r = Reader::new(&doc);
    assert!(r.try_bulk_with::<AsDegrees, _>(&mut out).unwrap());
    assert_eq!(out, degrees());
}

#[test]
fn a_block_read_that_runs_past_the_input_is_an_error() {
    let doc = to_beve(&plain());
    let mut r = Reader::new(&doc[..doc.len() - 1]);
    let mut out: Vec<Celsius> = Vec::new();
    assert_eq!(
        r.try_bulk_with::<AsDegrees, _>(&mut out).unwrap_err(),
        ErrorCode::UnexpectedEnd
    );
}

#[test]
fn a_user_type_can_borrow_a_block_out_of_the_document() {
    // `NumericBytes` is what `try_slice` is bounded on, so unsealing it opens
    // the no-copy path to a foreign scalar as well as the one-copy path.
    let doc = structio::to_beve_aligned(&plain());
    match beve::slice_ref::<Celsius>(&doc) {
        Some(block) => assert_eq!(block, degrees()),
        // The document did not land on an address `&[Celsius]` can point at.
        None => assert_eq!(from_beve::<Vec<f64>>(&doc).unwrap(), plain()),
    }
}
