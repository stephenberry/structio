//! The borrowing contract that holds on a big-endian target.
//!
//! [`tests/borrow.rs`](borrow.rs) covers the conditions a borrow has to meet
//! and is little-endian only, because on big-endian every one of them is
//! answered before it is asked: a borrow hands back the document's own bytes
//! reinterpreted as numbers, and the payload is little-endian, so
//! `Reader::borrow_block` declines outright.
//!
//! Declining is allowed to cost a copy. It is not allowed to cost an answer,
//! and that is the whole of what this file asserts.
#![cfg(target_endian = "big")]

use std::borrow::Cow;

use structio::beve::header;
use structio::{
    ErrorCode, beve_slice_ref, from_beve, read_beve_array_into, to_beve, to_beve_aligned,
    validate_beve,
};

#[test]
fn a_block_is_never_borrowed_and_always_read() {
    let samples = vec![1.5f64, -2.25, 3.5, 4.0];

    // The aligned form is the one written so that a borrow *could* happen. On
    // this target it still cannot, in either form.
    for doc in [to_beve(&samples), to_beve_aligned(&samples)] {
        assert!(
            beve_slice_ref::<f64>(&doc).is_none(),
            "borrowed a little-endian block on a big-endian target"
        );
        assert_eq!(from_beve::<Vec<f64>>(&doc).unwrap(), samples);
    }
}

#[test]
fn a_cow_field_takes_the_owned_half() {
    // The field that borrows where it can is the reason any of this exists.
    // Here it always owns, and the values have to survive the copy intact.
    let samples = vec![1.5f64, -2.25, 3.5, 4.0];
    let doc = to_beve_aligned(&samples);

    let read: Cow<'_, [f64]> = from_beve(&doc).unwrap();
    assert!(matches!(read, Cow::Owned(_)), "borrowed on big-endian");
    assert_eq!(read.as_ref(), samples.as_slice());
}

#[test]
fn a_padding_length_is_held_to_the_element_width_on_the_copying_paths_too() {
    // Declining the borrow sends every read down a path that copies and
    // swaps, and that path steps over the padding for itself. A length below
    // the element's width still reads, whatever the padding holds, and one at
    // or past it is still refused just past the length byte.
    let samples = [1.5f64, -2.25];
    let f64s = header::header(header::TY_TYPED_ARRAY, header::CAT_FLOAT, 3);
    for pad in [0usize, 7, 8, 9, 255] {
        let mut doc = vec![header::ALIGNED_ARRAY, f64s, 2 << 2, pad as u8];
        doc.extend(std::iter::repeat_n(0xAA, pad));
        for v in samples {
            doc.extend_from_slice(&v.to_le_bytes());
        }
        let mut streamed = Vec::<f64>::new();
        let reads = [
            ("validate", validate_beve(&doc)),
            ("Vec<f64>", from_beve::<Vec<f64>>(&doc).map(drop)),
            ("Cow<[f64]>", from_beve::<Cow<'_, [f64]>>(&doc).map(drop)),
            (
                "read_beve_array_into",
                read_beve_array_into(&mut streamed, &doc[..])
                    .map_err(|e| *e.as_parse().expect("a slice has no I/O to fail")),
            ),
        ];
        assert!(beve_slice_ref::<f64>(&doc).is_none(), "padding {pad}");
        if pad < 8 {
            for (name, r) in reads {
                r.unwrap_or_else(|e| panic!("{name}, padding {pad}: {e:?}"));
            }
            assert_eq!(from_beve::<Vec<f64>>(&doc).unwrap(), samples);
            assert_eq!(streamed, samples);
            continue;
        }
        for (name, r) in reads {
            let got = r.map_err(|e| (e.code, e.index));
            assert_eq!(
                got,
                Err((ErrorCode::InvalidPadding, 4)),
                "{name}, padding {pad}"
            );
        }
    }
}
