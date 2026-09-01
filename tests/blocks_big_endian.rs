//! The block contract that holds on a big-endian target.
//!
//! [`tests/blocks.rs`](blocks.rs) covers what an adapter has to do to reach
//! BEVE's typed-array path and is little-endian only, because on big-endian
//! the first test every bulk hook makes is the one that fails: a block is the
//! stored little-endian payload reinterpreted as values, so there is nothing
//! to reinterpret and every impl declines.
//!
//! Declining is allowed to cost a copy. It is not allowed to cost an answer,
//! and it is not allowed to cost the *bytes*, which is the half a reader on
//! another machine would notice. That is the whole of what this file asserts.
#![cfg(target_endian = "big")]

use structio::beve::{NumericBytes, Read, ReadAs, Reader, Write, WriteAs, Writer};
use structio::{ErrorCode, Options, beve_slice_ref, from_beve, to_beve, to_beve_aligned};

type PResult<T> = Result<T, ErrorCode>;

/// The same foreign scalar `tests/blocks.rs` adapts, and the same adapter,
/// down to the endianness tests that are the reason this file exists.
#[derive(Default, Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
struct Celsius(f64);

// SAFETY: `repr(transparent)` over `f64`, and the declared element is `f64`'s
// own. The little-endian clause is a claim about what BEVE stores, not about
// this host, so it holds here too -- it is why the hooks below decline rather
// than why they could not exist.
unsafe impl NumericBytes for Celsius {
    const ELEMENT: u8 = <f64 as NumericBytes>::ELEMENT;
}

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

#[test]
fn the_adapted_document_is_still_the_bare_type_s() {
    // `WriteAs::ARRAY` is a constant and has no endianness, so the typed array
    // is chosen here exactly as it is anywhere. Only the payload changes hands
    // differently, and both sides take the same byte-reversing arm.
    assert_eq!(
        to_beve(&Reading { samples: degrees() }),
        to_beve(&Plain { samples: plain() })
    );
}

#[test]
fn the_adapted_document_still_reads_back() {
    let doc = to_beve(&Plain { samples: plain() });
    assert_eq!(
        from_beve::<Reading>(&doc).unwrap(),
        Reading { samples: degrees() }
    );
}

#[test]
fn a_block_is_never_borrowed_by_a_user_type_either() {
    // Unsealing `NumericBytes` opened `try_slice` to a type declared outside
    // the crate. It did not open it to this target.
    for doc in [to_beve(&plain()), to_beve_aligned(&plain())] {
        assert!(
            beve_slice_ref::<Celsius>(&doc).is_none(),
            "borrowed a little-endian block on a big-endian target"
        );
        assert_eq!(from_beve::<Vec<f64>>(&doc).unwrap(), plain());
    }
}
